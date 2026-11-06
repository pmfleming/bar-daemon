//! Serialized, authorized UI intents. Lost replies never replay timezone writes.
use super::{
    Authority, Backend, Control, Fix, Source,
    policy::Zones,
    storage::{Accepted, State},
    wall_time,
};
use crate::location_place::Place;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub request_id: String,
    pub revision: u64,
    // None is explicitly Return to Automatic, not an unresolved city.
    pub place: Option<Place>,
}
impl Request {
    fn zone(&self, zones: &Zones) -> Result<Option<String>> {
        let Some(place) = &self.place else {
            return Ok(None);
        };
        place.validate()?;
        let fix = Fix {
            latitude: place.latitude,
            longitude: place.longitude,
            timestamp: wall_time(),
            source: Source::Manual,
            accuracy: 0.0,
        };
        let resolved = zones
            .at(&fix)
            .context("Place timezone is unresolved or near a boundary")?;
        ensure!(
            zones.canonical(&place.timezone).as_deref() == Some(&resolved),
            "Place coordinates and timezone disagree"
        );
        Ok(Some(resolved))
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Operation {
    pub uid: u32,
    pub request: Request,
    pub outcome: String,
}
pub(super) struct Command {
    pub uid: u32,
    pub request: Request,
    pub reply: oneshot::Sender<Result<String, String>>,
}

pub(super) fn status(state: &State) -> serde_json::Value {
    serde_json::json!({"available":true,"revision":state.accepted_revision,"manual":state.manual,
        "paused":state.paused,"uncertain":state.write_pending,"effective_place":state.manual_place,
        "operation_id":state.operation.as_ref().map(|o|o.request.request_id.as_str()),
        "operation_status":state.operation.as_ref().map(|o|o.outcome.as_str())})
}
impl Authority {
    pub(super) async fn apply_control(
        &mut self,
        backend: &impl Backend,
        control: &watch::Receiver<Control>,
        uid: u32,
        request: Request,
    ) -> Result<String> {
        ensure!(
            !request.request_id.is_empty()
                && request.request_id.len() <= 160
                && !request.request_id.chars().any(char::is_control),
            "Invalid operation ID"
        );
        if let Some(op) = &self.state.operation
            && op.uid == uid
            && op.request.request_id == request.request_id
        {
            ensure!(
                op.request == request,
                "Operation ID reused for different parameters"
            );
            ensure!(
                op.outcome == "applied",
                "Previous location operation is unconfirmed; inspect status"
            );
            return Ok(status(&self.state).to_string());
        }
        ensure!(
            request.revision == self.state.accepted_revision,
            "Location changed; refresh before saving"
        );
        ensure!(
            !self.state.paused && !self.state.write_pending,
            "Location safety pause requires administrative reconciliation"
        );
        let epoch = control.borrow().epoch;
        ensure!(
            control.borrow().active(epoch),
            "Location control unavailable while sleeping or stopping"
        );
        let current = backend.timezone().await?;
        ensure!(
            self.zones.canonical(&current) == self.zones.canonical(&self.baseline),
            "System timezone changed externally; reconcile before saving"
        );
        let zone = request.zone(&self.zones)?;
        let mut next = self.state.clone();
        next.accepted_revision = next
            .accepted_revision
            .checked_add(1)
            .context("Location revision exhausted")?;
        next.operation = Some(Operation {
            uid,
            request: request.clone(),
            outcome: if zone.is_some() { "pending" } else { "applied" }.into(),
        });
        if zone.is_none() {
            next.manual = false;
        } else {
            next.write_pending = true;
        }
        self.persist_control(next).await?;
        if let (Some(place), Some(zone)) = (request.place, zone) {
            let valid = control.borrow().active(epoch);
            let result = async {
                ensure!(valid, "Location control interrupted before apply");
                self.apply_timezone(backend, &current, &zone).await
            }
            .await;
            match result {
                Ok(actual) => {
                    let mut next = self.state.clone();
                    next.manual = true;
                    next.expected_timezone = Some(zone.clone());
                    next.accepted = Some(Accepted {
                        timezone: zone,
                        latitude: place.latitude,
                        longitude: place.longitude,
                        verified_at: wall_time(),
                        source: Source::Manual,
                        accuracy_meters: None,
                    });
                    next.manual_place = Some(place);
                    next.accepted_revision = next
                        .accepted_revision
                        .checked_add(1)
                        .context("Location revision exhausted")?;
                    next.write_pending = false;
                    next.operation
                        .as_mut()
                        .context("Missing location operation")?
                        .outcome = "applied".into();
                    self.persist_control(next).await?;
                    self.baseline = actual;
                }
                Err(error) => {
                    self.state.paused = true;
                    self.state
                        .operation
                        .as_mut()
                        .context("Missing location operation")?
                        .outcome = "uncertain".into();
                    self.storage.save(&self.state).await?;
                    self.status("Timezone change unverified · automatic location paused")
                        .await?;
                    return Err(error);
                }
            }
        } else {
            self.schedule.rearm();
        }
        self.status(if self.state.manual {
            "Current location set manually"
        } else {
            "Automatic location · awaiting fresh location"
        })
        .await?;
        Ok(status(&self.state).to_string())
    }
    async fn persist_control(&mut self, next: State) -> Result<()> {
        if let Err(error) = self.storage.save(&next).await {
            self.state.paused = true;
            self.state.write_pending = true;
            return Err(error);
        }
        self.state = next;
        Ok(())
    }
}
