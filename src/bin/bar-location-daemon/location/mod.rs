//! System-service location authority. Independent of the graphical session daemon.
mod bus;
#[cfg(test)]
mod bus_tests;
mod http;
mod policy;
mod reader;
mod storage;
#[cfg(test)]
mod tests;
mod wifi;

use anyhow::{Context, Result};
use policy::{Decision, Fix, Network, Schedule, Zones};
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
    async fn wifi(&self, started: f64) -> Result<http::LookupResult>;
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
}
impl Authority {
    async fn new(storage: Storage, zones: Zones, backend: &impl Backend) -> Result<Self> {
        let state = storage.load()?;
        storage.save(&state).await?; // migrate Python quota fields without losing accepted state/cooldowns
        Ok(Self {
            storage,
            state,
            zones,
            schedule: Schedule::default(),
            baseline: backend.timezone().await?,
            last_result: String::new(),
            reload: 0,
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
            if current.reload != self.reload {
                self.baseline = backend.timezone().await?;
                self.state.paused = false;
                self.state.write_pending = false;
                self.storage.save(&self.state).await?;
                self.schedule = Schedule::default();
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
                            // Connectivity flicker must not leave an obsolete offline
                            // warning on a completed, now-online connection.
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
                self.schedule
                    .retry(Instant::now(), self.state.cooldown(wall_time()));
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
        if !self.state.cooldown(wall_time()).is_zero() {
            return self
                .finish(Decision::Retry("Location provider rate limited"))
                .await;
        }
        self.storage.status("Checking current location…").await?;
        tracing::info!("acquiring filtered Wi-Fi and independent IP evidence");
        let started = wall_time();
        let epoch = control.borrow().epoch;
        let acquired = self.acquire(backend, control, started).await?;
        let (wifi, ip) = match acquired {
            Ok(evidence) => evidence,
            Err(decision) => return self.finish(decision).await,
        };
        let zone = match self.zones.agree(&wifi, &ip, started, wall_time()) {
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
        if current != self.baseline {
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
        // Check after persistence, immediately before the privileged call. Never
        // accept an acquisition from an old network or across a sleep/reload event.
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
                self.state.accepted = Some(Accepted {
                    timezone: zone,
                    latitude: wifi.latitude,
                    longitude: wifi.longitude,
                    verified_at: wall_time(),
                });
                self.state.write_pending = false;
                self.storage.save(&self.state).await?;
                tracing::info!(outcome = "accepted", verified_timezone = %self.baseline, timezone_changed = needs_write, current_location = "updated", retry = "new connection or explicit reload", "location check finished");
                self.schedule.complete();
                self.last_result.clear();
                self.storage.status("").await
            }
            Err(error) => {
                tracing::warn!(error = %error, "timezone application or verification failed");
                // Unknown application is not a transient lookup failure. Keep the
                // durable intent fence across process death; require explicit reload.
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
    ) -> Result<Result<(Fix, Fix), Decision>> {
        let wifi = backend.wifi(started);
        // Creating this async future does not issue a request. It is polled only
        // after usable Wi-Fi evidence and a fresh connection/control check.
        let ip = backend.ip();
        tokio::pin!(wifi, ip);
        let mut wifi_fix = None;
        let mut ip_fix = None;
        let mut ip_allowed = false;
        let epoch = control.borrow().epoch;
        let deadline = tokio::time::sleep(Duration::from_secs(90));
        tokio::pin!(deadline);
        let mut poll = tokio::time::interval(Duration::from_secs(5));
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            if wifi_fix.is_some() && !ip_allowed {
                let observed = backend.observe().await;
                let current = control.borrow();
                let valid = observed.is_ok_and(|s| {
                    !s.sleeping && s.network == self.schedule.network && s.network.is_some()
                }) && current.epoch == epoch
                    && !current.sleeping
                    && !current.shutdown
                    && Instant::now() < deadline.deadline();
                if !valid {
                    return Ok(Err(Decision::Retry("Location check interrupted")));
                }
                ip_allowed = true;
            }
            tokio::select! {
                // Persist a ready 429 before processing a simultaneous cancellation.
                biased;
                result = &mut ip, if ip_allowed && ip_fix.is_none() => match result {
                    http::LookupResult::Fix(fix) => {
                        tracing::info!(provider = "ipapi.co", reported_timezone = %fix.timezone, country_code = %fix.country_code, "IP evidence received");
                        ip_fix = Some(fix);
                    },
                    http::LookupResult::Limited(delay) => {
                        self.state.ip_not_before = self.state.ip_not_before.max(wall_time() + delay.as_secs_f64());
                        self.storage.save(&self.state).await?;
                        return Ok(Err(Decision::Retry("Location provider rate limited")));
                    }
                    http::LookupResult::Unavailable | http::LookupResult::NotRecognised =>
                        return Ok(Err(Decision::Retry("IP location unavailable"))),
                },
                result = &mut wifi, if wifi_fix.is_none() => match result {
                    Ok(http::LookupResult::Fix(fix)) => {
                        if !fix.usable_wifi(started, wall_time()) {
                            return Ok(Err(Decision::Retry("Fresh Wi-Fi location unavailable")));
                        }
                        tracing::info!(source = %fix.source, accuracy_meters = fix.accuracy, observation_age_seconds = wall_time() - fix.timestamp, "filtered Wi-Fi location found");
                        wifi_fix = Some(fix);
                    },
                    Ok(http::LookupResult::Limited(delay)) => {
                        self.state.wifi_not_before = self.state.wifi_not_before.max(wall_time() + delay.as_secs_f64());
                        self.storage.save(&self.state).await?;
                        return Ok(Err(Decision::Retry("Wi-Fi location provider rate limited")));
                    },
                    Ok(http::LookupResult::NotRecognised) =>
                        return Ok(Err(Decision::Stop("Wi-Fi location not recognised"))),
                    Ok(http::LookupResult::Unavailable) => return Ok(Err(Decision::Retry("Wi-Fi location unavailable"))),
                    Err(error) => {
                        tracing::warn!(error = %error, "Wi-Fi acquisition failed");
                        return Ok(Err(Decision::Retry("Wi-Fi location unavailable")));
                    },
                },
                changed = control.changed() => {
                    changed.context("location control ended")?;
                    return Ok(Err(Decision::Retry("Location check interrupted")));
                }
                _ = &mut deadline => return Ok(Err(Decision::Retry("Location acquisition timed out"))),
                _ = poll.tick() => {
                    let valid = backend.observe().await.is_ok_and(|s|
                        !s.sleeping && s.network == self.schedule.network && s.network.is_some());
                    if !valid { return Ok(Err(Decision::Retry("Network or sleep state changed"))); }
                }
            }
            if wifi_fix.is_some() && ip_fix.is_some() {
                return Ok(Ok((
                    wifi_fix.take().expect("checked Wi-Fi"),
                    ip_fix.take().expect("checked IP"),
                )));
            }
        }
    }
}

pub async fn run(
    state_directory: PathBuf,
    runtime_directory: PathBuf,
    tzdata: PathBuf,
) -> Result<()> {
    let zones = Zones::load(&tzdata).context("load timezone boundaries and country metadata")?;
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
