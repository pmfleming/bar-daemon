//! System-service location authority. Independent of the graphical session daemon.
mod bus;
#[cfg(test)]
mod bus_tests;
mod geoclue;
mod http;
mod policy;
mod reader;
mod storage;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use policy::{Decision, Fix, Network, Schedule, Source, Zones};
use std::{path::PathBuf, time::Duration};
use storage::{Accepted, State, Storage};
use tokio::{
    sync::watch,
    time::{Instant, MissedTickBehavior},
};

#[derive(Clone, Default)]
struct Control {
    epoch: u64,
    reload: u64,
    sleeping: bool,
    shutdown: bool,
}
struct Observation {
    network: Option<Network>,
    sleeping: bool,
}
trait Backend {
    async fn observe(&self) -> Result<Observation>;
    async fn timezone(&self) -> Result<String>;
    async fn set_timezone(&self, zone: &str) -> Result<()>;
    async fn geoclue(&self, started: f64) -> Result<Fix>;
    async fn ip(&self) -> http::LookupResult;
}
fn wall_time() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}

struct Authority {
    storage: Storage,
    state: State,
    zones: Zones,
    schedule: Schedule,
    baseline: String,
    last_result: String,
    reload: u64,
    control_epoch: u64,
}
impl Authority {
    async fn new(storage: Storage, zones: Zones, backend: &impl Backend) -> Result<Self> {
        let mut state = storage.load()?;
        let baseline = backend.timezone().await?;
        let canonical = zones
            .canonical(&baseline)
            .context("invalid system timezone")?;
        let expected = state
            .expected_timezone
            .as_deref()
            .or_else(|| state.accepted.as_ref().map(|a| a.timezone.as_str()));
        if let Some(expected) = expected {
            if zones.canonical(expected).as_deref() != Some(&canonical) {
                tracing::info!(current_timezone = %baseline, "timezone changed while authority was stopped; pausing");
                state.paused = true;
            }
        } else {
            // First installation has no evidence of a previous manual override.
            state.expected_timezone = Some(canonical);
        }
        storage.save(&state).await?;
        Ok(Self {
            storage,
            state,
            zones,
            schedule: Schedule::default(),
            baseline,
            last_result: String::new(),
            reload: 0,
            control_epoch: 0,
        })
    }
    async fn run(
        &mut self,
        backend: &impl Backend,
        mut control: watch::Receiver<Control>,
    ) -> Result<()> {
        let mut ticks = tokio::time::interval(Duration::from_secs(5));
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            let current = control.borrow_and_update().clone();
            if current.shutdown {
                return Ok(());
            }
            if current.epoch != self.control_epoch && current.reload == self.reload {
                // A coalesced sleep/resume must invalidate evidence even when
                // polling never observed the intermediate sleeping state.
                self.schedule.network_since = self.schedule.network.as_ref().map(|_| wall_time());
            }
            self.control_epoch = current.epoch;
            if current.reload != self.reload {
                self.baseline = backend.timezone().await?;
                self.state.expected_timezone = Some(
                    self.zones
                        .canonical(&self.baseline)
                        .context("invalid system timezone")?,
                );
                self.state.paused = false;
                self.state.write_pending = false;
                self.storage.save(&self.state).await?;
                self.schedule.rearm();
                self.reload = current.reload;
            }
            if self.state.paused || self.state.write_pending {
                self.storage
                    .status("Automatic location paused · reload to resume")
                    .await?;
            } else {
                match backend.observe().await {
                    Ok(observation) => {
                        let network = if observation.sleeping || current.sleeping {
                            None
                        } else {
                            observation.network
                        };
                        let changed = self.schedule.observe(network, Instant::now());
                        if self.schedule.network.is_none() {
                            self.storage
                                .status("Waiting for internet or resume · timezone unchanged")
                                .await?;
                        } else if self.schedule.ready(Instant::now()) {
                            self.check(backend, &mut control).await?;
                        } else if changed {
                            self.storage
                                .status(if self.schedule.due.is_some() {
                                    "Waiting to check current location…"
                                } else {
                                    &self.last_result
                                })
                                .await?;
                        }
                    }
                    Err(_) => {
                        self.schedule.observe(None, Instant::now());
                        self.storage
                            .status("Network observation unavailable · timezone unchanged")
                            .await?;
                    }
                }
            }
            tokio::select! {
                _ = ticks.tick() => {},
                changed = control.changed() => changed.context("location control ended")?,
            }
        }
    }
    async fn finish(&mut self, decision: Decision) -> Result<()> {
        let message = match decision {
            Decision::Retry(reason) => {
                // Provider cooldowns gate their own calls, not the whole pipeline.
                // In particular an IP checker cooldown cannot block GeoClue-IP.
                self.schedule
                    .retry(Instant::now(), self.state.geoclue_cooldown(wall_time()));
                tracing::info!(
                    reason,
                    outcome = "retry",
                    retry_in_seconds = self
                        .schedule
                        .due
                        .map(|due| due.saturating_duration_since(Instant::now()).as_secs()),
                    "location check finished"
                );
                format!("{reason} · timezone unchanged · retrying automatically")
            }
            Decision::Stop(reason) => {
                self.schedule.complete();
                tracing::info!(
                    reason,
                    outcome = "rejected",
                    retry = if self.state.paused || self.state.write_pending {
                        "explicit reload required"
                    } else {
                        "new connection or explicit reload"
                    },
                    "location check finished"
                );
                format!("{reason} · no automatic change")
            }
        };
        self.storage.status(&message).await?;
        self.last_result = message;
        Ok(())
    }
    #[tracing::instrument(skip_all, fields(attempt = wall_time()))]
    async fn check(
        &mut self,
        backend: &impl Backend,
        control: &mut watch::Receiver<Control>,
    ) -> Result<()> {
        // The default GeoClue endpoint is the retired direct beaconDB endpoint.
        // Honour an outstanding legacy cooldown rather than bypassing it on upgrade.
        if !self.state.geoclue_cooldown(wall_time()).is_zero() {
            return self
                .finish(Decision::Retry("GeoClue provider migration cooldown"))
                .await;
        }
        self.storage.status("Checking current location…").await?;
        tracing::info!("asking GeoClue; IP confirmation only for non-IP or unknown provenance");
        let started = self.schedule.network_since.unwrap_or_else(wall_time);
        let epoch = control.borrow().epoch;
        let acquired = self.acquire(backend, control, started).await?;
        // Persist an observed 429 before acting on any simultaneous cancellation.
        self.storage.save(&self.state).await?;
        let (geo, ip) = match acquired {
            Ok(evidence) => evidence,
            Err(decision) => return self.finish(decision).await,
        };
        let zone = match self.zones.decide(&geo, ip.as_ref(), started, wall_time()) {
            Ok(zone) => zone,
            Err(decision) => return self.finish(decision).await,
        };
        let current = match backend.timezone().await {
            Ok(current) => current,
            Err(_) => {
                return self
                    .finish(Decision::Retry("System timezone unavailable"))
                    .await;
            }
        };
        if self.zones.canonical(&current).is_none()
            || self.zones.canonical(&current) != self.zones.canonical(&self.baseline)
        {
            tracing::info!(baseline = %self.baseline, current_timezone = %current, proposed_timezone = %zone, "manual timezone change detected");
            self.state.paused = true;
            self.storage.save(&self.state).await?;
            return self
                .finish(Decision::Stop(
                    "Timezone changed manually · automatic location paused",
                ))
                .await;
        }
        self.state.write_pending = true;
        self.storage.save(&self.state).await?;
        // Recheck after persistence, immediately before the privileged call.
        let observed = backend.observe().await;
        let changed = control.borrow();
        let valid = observed.is_ok_and(|s| {
            !s.sleeping && s.network == self.schedule.network && s.network.is_some()
        }) && changed.epoch == epoch
            && !changed.sleeping
            && !changed.shutdown;
        drop(changed);
        if !valid {
            self.state.write_pending = false;
            self.storage.save(&self.state).await?;
            return self
                .finish(Decision::Retry("Location check interrupted"))
                .await;
        }
        let needs_write = self.zones.canonical(&current).as_deref() != Some(&zone);
        tracing::info!(current_timezone = %current, proposed_timezone = %zone, needs_write, "verifying accepted timezone");
        let applied = async {
            if needs_write {
                backend.set_timezone(&zone).await?;
            }
            let actual = backend.timezone().await?;
            anyhow::ensure!(
                self.zones.canonical(&actual).as_deref() == Some(&zone),
                "timezone not verified"
            );
            Ok::<_, anyhow::Error>(actual)
        }
        .await;
        match applied {
            Ok(actual) => {
                self.baseline = actual;
                self.state.expected_timezone = Some(zone.clone());
                self.state.accepted = Some(Accepted {
                    timezone: zone,
                    latitude: geo.latitude,
                    longitude: geo.longitude,
                    verified_at: wall_time(),
                    source: geo.source,
                    accuracy_meters: Some(geo.accuracy),
                });
                self.state.write_pending = false;
                self.storage.save(&self.state).await?;
                tracing::info!(outcome = "accepted", verified_timezone = %self.baseline,
                    timezone_changed = needs_write, source = ?geo.source, "location check finished");
                self.schedule.complete();
                self.last_result.clear();
                self.storage.status("").await
            }
            Err(error) => {
                tracing::warn!(error = %error, "timezone application or verification failed");
                // Keep durable intent across process death; explicit recovery only.
                self.state.paused = true;
                self.storage.save(&self.state).await?;
                self.finish(Decision::Stop(
                    "Timezone change unverified · automatic location paused",
                ))
                .await
            }
        }
    }
    async fn acquire(
        &mut self,
        backend: &impl Backend,
        control: &mut watch::Receiver<Control>,
        started: f64,
    ) -> Result<Result<(Fix, Option<Fix>), Decision>> {
        let epoch = control.borrow().epoch;
        let network = self.schedule.network.clone();
        let stage_control = control.clone();
        let deadline = tokio::time::sleep(Duration::from_secs(90));
        tokio::pin!(deadline);
        let deadline_at = deadline.deadline();
        let lookup = async {
            let geo = match backend.geoclue(started).await {
                Ok(fix) => fix,
                Err(error) => {
                    tracing::warn!(error = %error, "GeoClue acquisition failed");
                    return Ok(Err(Decision::Retry("GeoClue location unavailable")));
                }
            };
            if let Err(decision) = self.zones.geoclue_zone(&geo, started, wall_time()) {
                return Ok(Err(decision));
            }
            tracing::info!(source = ?geo.source, accuracy_meters = geo.accuracy,
                observation_age_seconds = wall_time() - geo.timestamp, "GeoClue evidence received");
            if geo.source == Source::Ip {
                return Ok(Ok((geo, None)));
            }
            if self.state.ip_not_before > wall_time() {
                return Ok(Err(Decision::Retry("IP confirmation rate limited")));
            }
            let observed = backend.observe().await;
            let current = stage_control.borrow();
            let valid = observed
                .is_ok_and(|s| !s.sleeping && s.network == network && s.network.is_some())
                && current.epoch == epoch
                && !current.sleeping
                && !current.shutdown
                && Instant::now() < deadline_at;
            drop(current);
            if !valid {
                return Ok(Err(Decision::Retry("Location check interrupted")));
            }
            // Sequential by construction; no IP request for GeoClue-IP results.
            match backend.ip().await {
                http::LookupResult::Fix(ip) => Ok(Ok((geo, Some(ip)))),
                http::LookupResult::Limited(delay) => {
                    self.state.ip_not_before = self
                        .state
                        .ip_not_before
                        .max(wall_time() + delay.as_secs_f64());
                    // No await after observing 429: the outer caller persists it
                    // outside the cancellable future before processing the result.
                    Ok(Err(Decision::Retry("IP confirmation rate limited")))
                }
                http::LookupResult::Unavailable => {
                    Ok(Err(Decision::Retry("IP confirmation unavailable")))
                }
            }
        };
        tokio::pin!(lookup);
        let network_watch = async {
            let period = Duration::from_secs(5);
            let mut poll = tokio::time::interval_at(Instant::now() + period, period);
            poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                poll.tick().await;
                let valid = backend
                    .observe()
                    .await
                    .is_ok_and(|s| !s.sleeping && s.network == network && s.network.is_some());
                if !valid {
                    return Decision::Retry("Network or sleep state changed");
                }
            }
        };
        tokio::pin!(network_watch);
        loop {
            tokio::select! {
                biased;
                result = &mut lookup => return result,
                changed = control.changed() => {
                    changed.context("location control ended")?;
                    return Ok(Err(Decision::Retry("Location check interrupted")));
                }
                _ = &mut deadline => return Ok(Err(Decision::Retry("Location acquisition timed out"))),
                decision = &mut network_watch => return Ok(Err(decision)),
            }
        }
    }
}

pub async fn run(
    state_directory: PathBuf,
    runtime_directory: PathBuf,
    tzdata: PathBuf,
) -> Result<()> {
    let zones = Zones::load(&tzdata).context("load timezone boundaries and aliases")?;
    let connection = tokio::time::timeout(
        Duration::from_secs(10),
        zbus::connection::Builder::system()?
            .method_timeout(Duration::from_secs(5))
            .serve_at(
                "/org/laufan/LocationTimezone",
                reader::Reader(Storage::new(
                    state_directory.clone(),
                    runtime_directory.clone(),
                )),
            )?
            .name("org.laufan.LocationTimezone")?
            .build(),
    )
    .await??;
    let backend = bus::Bus {
        connection,
        http: http::client()?,
    };
    let mut authority = Authority::new(
        Storage::new(state_directory, runtime_directory),
        zones,
        &backend,
    )
    .await?;
    let (sender, receiver) = watch::channel(Control::default());
    let events = bus::events(&backend.connection, sender);
    tokio::pin!(events);
    let running = authority.run(&backend, receiver);
    tokio::pin!(running);
    tokio::select! {
        result = &mut running => result,
        result = &mut events => {
            result?;
            // Allow in-flight writes to read back/persist their outcome on SIGTERM.
            tokio::time::timeout(Duration::from_secs(15), &mut running).await.context("location shutdown deadline")?
        }
    }
}
