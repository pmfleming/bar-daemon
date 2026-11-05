//! Serialized workspace lifecycle. QML consumes the projection, never this policy.
mod fixtures;
mod handoff;
mod journal;
mod presentation;
mod replies;
#[cfg(test)]
mod tests;

use anyhow::{Context as _, Result, bail, ensure};
use journal::{CloseRecord, Entry, Journal, Reservation, Target};
use serde_json::{Value, json};
use shelllist_daemon_core::workspace_wire::{
    self as wire, Context, Create, Lease, Placement, Purpose,
};
use shelllist_hyprland::{Client, workspace as compositor};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub(crate) use fixtures::control_fixtures;
pub(crate) use presentation::View;
use replies::{CommandReply, ContextReply, FocusReply, LeaseReply, RecordedReply};

fn enabled() -> bool {
    std::env::var("SHELLLIST_TEMPORARY_WORKSPACES").as_deref() != Ok("0")
}

#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Command {
    command: CommandKind,
    #[serde(default)]
    workspace_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_position: Option<i64>,
    #[serde(default)]
    monitor: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
}

#[derive(
    Debug, Clone, Copy, serde::Serialize, serde::Deserialize, schemars::JsonSchema, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum CommandKind {
    Create,
    Close,
    Next,
    Previous,
    Focus,
    Move,
}

pub async fn run_command(
    command: CommandKind,
    monitor: Option<String>,
    workspace_id: Option<i64>,
    position: bool,
) -> Result<()> {
    let connection = crate::bus::session().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        crate::api::BUS_NAME,
        crate::api::OBJECT_PATH,
        crate::api::INTERFACE,
    )
    .await?;
    let mut request = json!({"command":command, "monitor":monitor, "request_id":crate::state::publication::Publication::new().epoch});
    request[if position {
        "workspace_position"
    } else {
        "workspace_id"
    }] = workspace_id.into();
    // A timeout is an unknown outcome; this CLI never retries an effect.
    let reply: String = proxy
        .call("Call", &("workspace.command", request.to_string()))
        .await?;
    let reply: Value = serde_json::from_str(&reply)?;
    ensure!(
        reply["ok"] == true,
        "workspace command failed: {}",
        reply["error"]
    );
    println!("{}", reply["data"]["workspace_operation"]);
    Ok(())
}

pub(crate) fn policy(method: &str) -> shelllist_daemon_core::MethodPolicy {
    use shelllist_daemon_core::{ExecutionMode, MethodPolicy, Sensitivity};
    let read = matches!(method, "workspace.context" | "workspace.operation.get");
    let mut policy = MethodPolicy::new(
        if read {
            ExecutionMode::Read
        } else {
            ExecutionMode::Effect
        },
        if read {
            Sensitivity::SensitiveRead
        } else {
            Sensitivity::Effect
        },
        "workspaces",
    );
    policy.max_in_flight = 4;
    policy.request_bytes = 16 * 1024;
    policy
}

pub(crate) fn contract<P: schemars::JsonSchema>(method: &str) -> Value {
    let mut contract = match method {
        "workspace.context" => shelllist_daemon_core::method_schemas::<P, ContextReply>(),
        "workspace.create" => shelllist_daemon_core::method_schemas::<P, Reservation>(),
        "workspace.close" => shelllist_daemon_core::method_schemas::<P, CloseRecord>(),
        "workspace.placement.apply" | "workspace.lease.release" => {
            shelllist_daemon_core::method_schemas::<P, LeaseReply>()
        }
        "workspace.command" => shelllist_daemon_core::method_schemas::<P, replies::CommandReply>(),
        "workspace.operation.get" => {
            shelllist_daemon_core::method_schemas::<P, replies::RecordedReply>()
        }
        _ => unreachable!("unregistered workspace contract"),
    };
    if matches!(method, "workspace.command" | "workspace.operation.get") {
        // Every variant requires status. Keep this common invariant at the
        // union root as well as inside each generated variant schema.
        contract["result_schema"]["required"] = json!(["status"]);
    }
    contract["name"] = method.into();
    contract["response_key"] = "workspace_operation".into();
    contract["policy"] = serde_json::to_value(policy(method)).unwrap();
    contract
}

struct Loaded {
    journal: Journal,
    path: PathBuf,
    poisoned: bool,
    empty_since: handoff::EmptySince,
}

pub(crate) struct Service {
    pub(crate) changed: Arc<tokio::sync::Notify>,
    client: Client,
    loaded: Mutex<Option<Loaded>>,
    writer: String,
    journal_root: Option<PathBuf>,
    handoff_events: std::sync::Mutex<handoff::Events>,
}

impl Default for Service {
    fn default() -> Self {
        Self {
            changed: Arc::new(tokio::sync::Notify::new()),
            client: Client::default(),
            loaded: Mutex::new(None),
            writer: crate::state::publication::Publication::new().epoch,
            journal_root: None,
            handoff_events: Default::default(),
        }
    }
}

impl Loaded {
    async fn save(&mut self) -> Result<()> {
        ensure!(
            !self.poisoned,
            "workspace journal requires restart after persistence failure"
        );
        if let Err(error) = self.journal.save(&self.path).await {
            self.poisoned = true;
            return Err(error);
        }
        Ok(())
    }
}

// Owned by the sole bar daemon within one exact compositor session.
const PIN_PREFIX: &str = "shelllist-bar-workspace-";

impl Service {
    async fn initialize<'a>(
        &self,
        slot: &'a mut Option<Loaded>,
        snapshot: &compositor::Snapshot,
    ) -> Result<&'a mut Loaded> {
        if slot.is_none() {
            let root = self
                .journal_root
                .clone()
                .or_else(|| std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
                .context("runtime directory required")?;
            // instance_signature has already validated this path component.
            let path = root
                .join("bar-daemon")
                .join(format!("workspaces-{}.json", snapshot.session));
            let journal = Journal::load(&path, &snapshot.session)?;
            // Pinning precedes durable close admission, so a crash can leave
            // references with no journal record. Clear only this owner's prefix
            // before any new pins are admitted; restarted observers are fenced.
            compositor::execute(&self.client, &compositor::release_prefix(PIN_PREFIX)?).await?;
            *slot = Some(Loaded {
                journal,
                path,
                poisoned: false,
                empty_since: Default::default(),
            });
            slot.as_mut().unwrap().save().await?;
        }
        let loaded = slot.as_mut().unwrap();
        ensure!(!loaded.poisoned, "workspace journal unavailable");
        ensure!(
            loaded.journal.session == snapshot.session,
            "compositor session changed"
        );
        Ok(loaded)
    }

    pub async fn dispatch(self: &Arc<Self>, method: &str, params: Value) -> Value {
        let result = self.call(method, params).await;
        if !matches!(method, "workspace.context" | "workspace.operation.get") {
            self.changed.notify_one();
        }
        match result {
            Ok(value) => crate::protocol::success(json!({"workspace_operation": value})),
            Err(error) => {
                crate::protocol::error("workspace-operation-failed", format!("{error:#}"))
            }
        }
    }

    async fn call(self: &Arc<Self>, method: &str, params: Value) -> Result<Value> {
        let mut guard = self.loaded.lock().await;
        let snapshot = compositor::snapshot(&self.client).await?;
        let loaded = self.initialize(&mut guard, &snapshot).await?;
        match method {
            "workspace.command" => {
                let command: Command = serde_json::from_value(params)?;
                ensure!(
                    if matches!(command.command, CommandKind::Focus | CommandKind::Move) {
                        command.workspace_id.is_some() ^ command.workspace_position.is_some()
                    } else {
                        command.workspace_id.is_none() && command.workspace_position.is_none()
                    },
                    "only focus and move require exactly one workspace ID or bar position"
                );
                if let Some(key) = &command.request_id {
                    if let Some(record) = loaded.journal.reservations.get(key) {
                        ensure!(
                            matches!(command.command, CommandKind::Create)
                                && record.request.purpose == Purpose::LandingPad
                                && command
                                    .monitor
                                    .as_ref()
                                    .is_none_or(|m| m == &record.request.context.monitor),
                            "request key reused with different command"
                        );
                        return Ok(serde_json::to_value(record)?);
                    }
                    if let Some(record) = loaded.journal.closes.get(key) {
                        ensure!(
                            matches!(command.command, CommandKind::Close)
                                && command
                                    .monitor
                                    .as_ref()
                                    .is_none_or(|m| m == &record.request.context.monitor),
                            "request key reused with different command"
                        );
                        return Ok(serde_json::to_value(record)?);
                    }
                }
                let monitor = snapshot
                    .monitors
                    .iter()
                    .find(|m| {
                        command
                            .monitor
                            .as_ref()
                            .map_or(m.focused, |name| &m.name == name)
                    })
                    .context("invoking monitor unavailable")?;
                let context = Context {
                    session: snapshot.session.clone(),
                    monitor: monitor.name.clone(),
                    workspace_id: monitor
                        .active_workspace
                        .id
                        .filter(|id| *id > 0)
                        .context("ordinary workspace required")?,
                };
                let key = command
                    .request_id
                    .unwrap_or_else(|| crate::state::publication::Publication::new().epoch);
                match command.command {
                    CommandKind::Focus | CommandKind::Move => {
                        let id = if let Some(position) = command.workspace_position {
                            let index = usize::try_from(position)
                                .ok()
                                .and_then(|position| position.checked_sub(1))
                                .context("invalid workspace bar position")?;
                            *presentation::workspace_ids(
                                &loaded.journal,
                                &context.monitor,
                                snapshot
                                    .workspaces
                                    .iter()
                                    .filter_map(|w| Some((w.id?, w.monitor.as_str()))),
                            )
                            .get(index)
                            .context("workspace bar position does not exist on this monitor")?
                        } else {
                            command.workspace_id.unwrap()
                        };
                        ensure!(
                            (1..=wire::MAX_WORKSPACE_ID).contains(&id),
                            "invalid workspace ID"
                        );
                        ensure!(!loaded.journal.closing(id), "workspace is closing");
                        ensure!(
                            !snapshot
                                .workspaces
                                .iter()
                                .any(|w| w.id == Some(id) && w.monitor != context.monitor),
                            "workspace belongs to another monitor"
                        );
                        let retained =
                            loaded.journal.entries.get(&id).is_some_and(|e| {
                                e.keep_until_closed && e.monitor == context.monitor
                            });
                        let materialize = id <= 5 || retained;
                        ensure!(
                            materialize
                                || snapshot
                                    .workspaces
                                    .iter()
                                    .any(|w| w.id == Some(id) && w.monitor == context.monitor),
                            "workspace does not exist on this monitor"
                        );
                        compositor::execute(
                            &self.client,
                            &compositor::select_local(
                                &context.monitor,
                                context.workspace_id,
                                id,
                                matches!(command.command, CommandKind::Move),
                                materialize,
                            )?,
                        )
                        .await?;
                        if matches!(command.command, CommandKind::Move)
                            && !snapshot
                                .windows
                                .iter()
                                .any(|w| w.mapped && w.workspace.id == Some(id))
                            && let Some(entry) = loaded.journal.entries.get_mut(&id)
                        {
                            // Preserve the observed empty transition even if event
                            // coalescing sees only the newly occupied destination.
                            entry.cue_empty = true;
                            loaded.save().await?;
                        }
                        Ok(serde_json::to_value(CommandReply::Focus(FocusReply {
                            status: if matches!(command.command, CommandKind::Move) {
                                "moved"
                            } else {
                                "focused"
                            }
                            .into(),
                            workspace_id: id,
                        }))?)
                    }
                    CommandKind::Create => {
                        self.create(
                            loaded,
                            &snapshot,
                            Create {
                                request_id: key,
                                context,
                                purpose: Purpose::LandingPad,
                                workspace_id: None,
                                origin: "Workspace".into(),
                                icon: "workspace".into(),
                            },
                        )
                        .await
                    }
                    CommandKind::Close => {
                        self.close(
                            loaded,
                            &snapshot,
                            wire::Close {
                                request_id: key,
                                context,
                            },
                        )
                        .await
                    }
                    CommandKind::Next | CommandKind::Previous => {
                        let mut ids = vec![1, 2, 3, 4, 5];
                        ids.extend(
                            snapshot
                                .workspaces
                                .iter()
                                .filter(|w| {
                                    w.id.is_some_and(|id| id > 5) && w.monitor == context.monitor
                                })
                                .filter_map(|w| w.id),
                        );
                        ids.extend(
                            loaded
                                .journal
                                .entries
                                .values()
                                .filter(|e| e.monitor == context.monitor)
                                .map(|e| e.id),
                        );
                        ids.sort_unstable();
                        ids.dedup();
                        // Navigation never migrates a workspace from another monitor.
                        ids.retain(|id| {
                            !snapshot
                                .workspaces
                                .iter()
                                .any(|w| w.id == Some(*id) && w.monitor != context.monitor)
                        });
                        ensure!(!ids.is_empty(), "no local workspace");
                        let position = ids
                            .iter()
                            .position(|id| *id == context.workspace_id)
                            .unwrap_or(0);
                        let index = if matches!(command.command, CommandKind::Next) {
                            (position + 1) % ids.len()
                        } else {
                            (position + ids.len() - 1) % ids.len()
                        };
                        compositor::execute(
                            &self.client,
                            &compositor::focus_local(
                                &context.monitor,
                                context.workspace_id,
                                ids[index],
                                true,
                            )?,
                        )
                        .await?;
                        Ok(serde_json::to_value(CommandReply::Focus(FocusReply {
                            status: "focused".into(),
                            workspace_id: ids[index],
                        }))?)
                    }
                }
            }
            "workspace.context" => {
                let monitor = params.get("monitor").and_then(Value::as_str);
                let monitor = snapshot
                    .monitors
                    .iter()
                    .find(|m| monitor.map_or(m.focused, |name| m.name == name))
                    .context("invoking monitor unavailable")?;
                ensure!(
                    monitor.active_workspace.id.is_some_and(|id| id > 0),
                    "ordinary workspace required"
                );
                Ok(serde_json::to_value(ContextReply {
                    version: wire::VERSION,
                    context: Context {
                        session: snapshot.session,
                        monitor: monitor.name.clone(),
                        workspace_id: monitor
                            .active_workspace
                            .id
                            .filter(|id| *id > 0)
                            .context("ordinary workspace required")?,
                    },
                    request_id: crate::state::publication::Publication::new().epoch,
                })?)
            }
            "workspace.create" => {
                self.create(loaded, &snapshot, serde_json::from_value(params)?)
                    .await
            }
            "workspace.placement.apply" => {
                self.place(loaded, &snapshot, serde_json::from_value(params)?)
                    .await
            }
            "workspace.lease.release" => {
                let lease: Lease = serde_json::from_value(params)?;
                self.check_lease(loaded, &lease, false)?;
                let reservation = loaded
                    .journal
                    .reservations
                    .get_mut(&lease.request_id)
                    .unwrap();
                reservation.released = true;
                if reservation.status != "unknown" {
                    reservation.status = "released".into();
                }
                loaded.save().await?;
                self.cleanup(loaded, &snapshot).await?;
                Ok(serde_json::to_value(LeaseReply {
                    status: "released".into(),
                    lease,
                })?)
            }
            "workspace.operation.get" => {
                let query: wire::Lookup = serde_json::from_value(params)?;
                ensure!(query.session == snapshot.session, "stale session");
                if let Some(record) = loaded.journal.reservations.get(&query.request_id) {
                    return Ok(serde_json::to_value(RecordedReply::Reservation(record))?);
                }
                if let Some(record) = loaded.journal.closes.get(&query.request_id) {
                    return Ok(serde_json::to_value(RecordedReply::Close(record))?);
                }
                bail!("workspace operation not found")
            }
            "workspace.close" => {
                self.close(loaded, &snapshot, serde_json::from_value(params)?)
                    .await
            }
            _ => bail!("unsupported workspace method"),
        }
    }

    async fn create(
        &self,
        loaded: &mut Loaded,
        snapshot: &compositor::Snapshot,
        request: Create,
    ) -> Result<Value> {
        ensure!(
            enabled() || (request.purpose == Purpose::Launch && request.workspace_id.is_some()),
            "temporary workspace creation disabled"
        );
        if let Some(existing) = loaded.journal.reservations.get(&request.request_id) {
            ensure!(
                existing.request == request,
                "request key reused with different parameters"
            );
            return Ok(serde_json::to_value(existing)?);
        }
        validate_context(
            snapshot,
            &request.context,
            request.purpose == Purpose::LandingPad,
        )?;
        ensure!(
            request.origin.len() <= 256 && request.icon.len() <= 256,
            "origin exceeds limit"
        );
        loaded.journal.admit(&request.request_id)?;
        let id = match request.workspace_id {
            Some(id) => {
                ensure!(
                    (1..=wire::MAX_WORKSPACE_ID).contains(&id),
                    "invalid workspace ID"
                );
                ensure!(
                    id <= 5
                        || snapshot.workspaces.iter().any(|w| w.id == Some(id))
                        || loaded.journal.entries.get(&id).is_some_and(|entry| {
                            entry.keep_until_closed && entry.monitor == request.context.monitor
                        }),
                    "destination disappeared"
                );
                id
            }
            None => loaded
                .journal
                .allocate(snapshot.workspaces.iter().filter_map(|w| w.id))?,
        };
        ensure!(!loaded.journal.closing(id), "destination is closing");
        let generation = loaded
            .journal
            .entries
            .get(&id)
            .map(|e| e.generation.clone())
            .unwrap_or_else(|| crate::state::publication::Publication::new().epoch);
        let lease = Lease {
            session: snapshot.session.clone(),
            request_id: request.request_id.clone(),
            workspace_id: id,
            generation: generation.clone(),
        };
        if request.workspace_id.is_none() {
            loaded.journal.entries.insert(
                id,
                Entry {
                    id,
                    generation,
                    monitor: request.context.monitor.clone(),
                    occupied: false,
                    landing_pad: request.purpose == Purpose::LandingPad,
                    keep_until_closed: request.purpose == Purpose::LandingPad,
                    origin: request.origin.clone(),
                    icon: request.icon.clone(),
                    cue_assigned: request.purpose == Purpose::Launch,
                    cue_empty: true,
                },
            );
        }
        let reservation = Reservation {
            request: request.clone(),
            lease: lease.clone(),
            writer: self.writer.clone(),
            released: request.purpose == Purpose::LandingPad,
            placed: false,
            placement: None,
            status: "reserved".into(),
        };
        loaded
            .journal
            .reservations
            .insert(request.request_id.clone(), reservation);
        loaded.save().await?;
        if request.purpose == Purpose::LandingPad {
            let result = async {
                compositor::execute(
                    &self.client,
                    &compositor::focus_local(
                        &request.context.monitor,
                        request.context.workspace_id,
                        id,
                        true,
                    )?,
                )
                .await?;
                let observed = compositor::snapshot(&self.client).await?;
                ensure!(
                    observed
                        .monitors
                        .iter()
                        .any(|m| m.name == request.context.monitor
                            && m.active_workspace.id == Some(id)),
                    "workspace focus was not observed"
                );
                Ok::<_, anyhow::Error>(())
            }
            .await;
            let record = loaded
                .journal
                .reservations
                .get_mut(&request.request_id)
                .unwrap();
            record.status = if result.is_ok() { "focused" } else { "unknown" }.into();
            loaded.save().await?;
            result?;
        }
        Ok(serde_json::to_value(
            &loaded.journal.reservations[&request.request_id],
        )?)
    }

    fn check_lease(&self, loaded: &Loaded, lease: &Lease, effect: bool) -> Result<()> {
        ensure!(
            lease.session == loaded.journal.session,
            "stale lease session"
        );
        let record = loaded
            .journal
            .reservations
            .get(&lease.request_id)
            .context("unknown lease")?;
        ensure!(&record.lease == lease, "stale lease identity");
        if effect {
            ensure!(
                !record.released && record.writer == self.writer,
                "placement writer fenced"
            );
        }
        Ok(())
    }

    async fn place(
        &self,
        loaded: &mut Loaded,
        snapshot: &compositor::Snapshot,
        request: Placement,
    ) -> Result<Value> {
        self.check_lease(loaded, &request.lease, true)?;
        let record = &loaded.journal.reservations[&request.lease.request_id];
        if record.placed {
            ensure!(
                record.placement.as_ref() == Some(&request),
                "placement parameters changed"
            );
            return Ok(serde_json::to_value(LeaseReply {
                status: "placed".into(),
                lease: request.lease,
            })?);
        }
        ensure!(
            record.status == "reserved",
            "placement outcome requires reconciliation; never replay"
        );
        ensure!(
            snapshot.windows.iter().any(|w| w.mapped
                && w.address == request.window.address
                && w.pid == request.window.pid
                && w.workspace.id == Some(request.window.workspace_id)),
            "stale placement candidate"
        );
        ensure!(
            snapshot
                .monitors
                .iter()
                .any(|m| m.name == record.request.context.monitor),
            "destination monitor disappeared"
        );
        let context = record.request.context.clone();
        let id = request.lease.workspace_id;
        ensure!(!loaded.journal.closing(id), "workspace is closing");
        // A new explicit launch from an existing process supersedes any older
        // replacement intent for that process; observers must never compete.
        let superseded: Vec<_> = loaded
            .journal
            .handoffs
            .iter()
            .filter(|(_, h)| h.window.pid == request.window.pid)
            .map(|(key, _)| key.clone())
            .collect();
        for key in superseded {
            self.cancel_handoff(loaded, &key).await?;
        }
        let handoff = handoff::Handoff::prepare(loaded, &request, snapshot);
        if let Some(handoff) = &handoff {
            loaded
                .journal
                .handoffs
                .insert(request.lease.request_id.clone(), handoff.clone());
        }
        loaded
            .journal
            .reservations
            .get_mut(&request.lease.request_id)
            .unwrap()
            .status = "placing".into();
        loaded
            .journal
            .reservations
            .get_mut(&request.lease.request_id)
            .unwrap()
            .placement = Some(request.clone());
        loaded.save().await?;
        let destination_monitor = snapshot
            .workspaces
            .iter()
            .find(|w| w.id == Some(id))
            .map_or(context.monitor.as_str(), |w| w.monitor.as_str());
        let mut expression = compositor::move_local(
            &request.window.address,
            request.window.pid,
            request.window.workspace_id,
            id,
            destination_monitor,
        )?;
        if let Some(handoff) = &handoff {
            expression = expression.then(compositor::pin_window(
                &handoff.token,
                &request.window.address,
                request.window.pid,
                id,
            )?);
        }
        let result = compositor::execute(&self.client, &expression).await;
        let current = compositor::snapshot(&self.client).await?;
        let verified = result.is_ok()
            && current.windows.iter().any(|w| {
                w.mapped
                    && w.address == request.window.address
                    && w.pid == request.window.pid
                    && w.workspace.id == Some(id)
            });
        let record = loaded
            .journal
            .reservations
            .get_mut(&request.lease.request_id)
            .unwrap();
        record.placed = verified;
        record.status = if verified { "placed" } else { "unknown" }.into();
        if verified
            && let Some(handoff) = loaded.journal.handoffs.get_mut(&request.lease.request_id)
        {
            handoff.watching = true;
        }
        if verified && let Some(entry) = loaded.journal.entries.get_mut(&id) {
            entry.occupied = true;
            if !entry.cue_assigned || entry.cue_empty {
                let origin = &loaded.journal.reservations[&request.lease.request_id].request;
                entry.origin = origin.origin.clone();
                entry.icon = origin.icon.clone();
                entry.cue_assigned = true;
            }
            entry.cue_empty = false;
        }
        loaded.save().await?;
        if !verified {
            self.cancel_handoff(loaded, &request.lease.request_id)
                .await?;
        }
        ensure!(
            verified,
            "placement not observed; launch must not be replayed"
        );
        if request.follow
            && validate_context(&current, &context, true).is_ok()
            && current
                .monitors
                .iter()
                .any(|m| m.focused && m.name == context.monitor)
        {
            compositor::execute(
                &self.client,
                &compositor::follow_window(
                    &request.window.address,
                    request.window.pid,
                    id,
                    &context.monitor,
                    context.workspace_id,
                )?,
            )
            .await?;
        }
        Ok(serde_json::to_value(LeaseReply {
            status: "placed".into(),
            lease: request.lease,
        })?)
    }

    async fn close(
        self: &Arc<Self>,
        loaded: &mut Loaded,
        snapshot: &compositor::Snapshot,
        request: wire::Close,
    ) -> Result<Value> {
        if let Some(record) = loaded.journal.closes.get(&request.request_id) {
            ensure!(
                record.request == request,
                "request key reused with different parameters"
            );
            return Ok(serde_json::to_value(record)?);
        }
        validate_context(snapshot, &request.context, true)?;
        if let Some(record) = loaded.journal.closes.values().find(|r| {
            r.request.context.workspace_id == request.context.workspace_id
                && matches!(r.status.as_str(), "dispatching" | "waiting")
        }) {
            return Ok(serde_json::to_value(record)?);
        }
        loaded.journal.admit(&request.request_id)?;
        let prefix = format!(
            "{PIN_PREFIX}close-{}",
            crate::state::publication::Publication::new().epoch
        );
        let snapshot = compositor::pinned_snapshot(
            &self.client,
            &request.context.monitor,
            request.context.workspace_id,
            &prefix,
        )
        .await?;
        let targets: Vec<_> = snapshot
            .windows
            .iter()
            .filter(|w| w.mapped && w.workspace.id == Some(request.context.workspace_id))
            .map(|w| Target {
                window: wire::WindowIdentity {
                    address: w.address.clone(),
                    pid: w.pid,
                    workspace_id: request.context.workspace_id,
                },
                token: format!("{prefix}-{}", w.address),
                attempted: false,
            })
            .collect();
        ensure!(targets.len() <= 512, "workspace window limit exceeded");
        let key = request.request_id.clone();
        loaded.journal.closes.insert(
            key.clone(),
            CloseRecord {
                request,
                targets: targets.clone(),
                status: "dispatching".into(),
                error: None,
            },
        );
        // Admission fences every pending writer for this destination before the
        // first close. A late window may map, but cannot revive placement here.
        let closing_id = loaded.journal.closes[&key].request.context.workspace_id;
        for handoff in loaded
            .journal
            .handoffs
            .values_mut()
            .filter(|h| h.window.workspace_id == closing_id)
        {
            handoff.watching = false;
        }
        for reservation in loaded
            .journal
            .reservations
            .values_mut()
            .filter(|r| r.lease.workspace_id == closing_id && !r.released)
        {
            reservation.released = true;
            reservation.status = "released".into();
        }
        // Persist explicit retirement with admission, before any window close.
        if let Some(entry) = loaded.journal.entries.get_mut(&closing_id) {
            entry.landing_pad = false;
            entry.keep_until_closed = false;
        }
        loaded.save().await?;
        for (index, target) in targets.iter().enumerate() {
            let w = &target.window;
            loaded.journal.closes.get_mut(&key).unwrap().targets[index].attempted = true;
            loaded.save().await?;
            if let Err(error) = compositor::execute(
                &self.client,
                &compositor::close_pinned(&target.token, &w.address, w.pid, w.workspace_id)?,
            )
            .await
            {
                loaded.journal.closes.get_mut(&key).unwrap().error = Some(error.to_string());
            }
            let _ = compositor::execute(&self.client, &compositor::release_window(&target.token)?)
                .await;
        }
        loaded.journal.closes.get_mut(&key).unwrap().status = "waiting".into();
        loaded.save().await?;
        let service = Arc::clone(self);
        let observe_key = key.clone();
        tokio::spawn(async move {
            service.observe_close(&observe_key).await;
        });
        Ok(serde_json::to_value(&loaded.journal.closes[&key])?)
    }

    async fn observe_close(&self, key: &str) {
        for attempt in 0..26 {
            let mut guard = self.loaded.lock().await;
            let snapshot = compositor::snapshot(&self.client).await;
            let Some(loaded) = guard.as_mut() else { return };
            let Some(record) = loaded.journal.closes.get_mut(key) else {
                return;
            };
            let remaining = snapshot.as_ref().is_ok_and(|s| {
                s.windows.iter().any(|w| {
                    w.mapped
                        && record
                            .targets
                            .iter()
                            .any(|t| t.window.address == w.address && t.window.pid == w.pid)
                })
            });
            if snapshot.is_err() || !remaining || attempt == 25 {
                record.status = if snapshot.is_err() {
                    "unknown"
                } else if remaining {
                    "still-open"
                } else {
                    "closed"
                }
                .into();
                if loaded.save().await.is_ok()
                    && let Ok(snapshot) = snapshot
                {
                    let _ = self.cleanup(loaded, &snapshot).await;
                }
                self.changed.notify_one();
                return;
            }
            drop(guard);
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn cleanup(&self, loaded: &mut Loaded, snapshot: &compositor::Snapshot) -> Result<()> {
        // A replacement changes occupancy; never retire against the old snapshot.
        let refreshed;
        let snapshot = if self.reconcile_handoffs(loaded, snapshot).await? {
            refreshed = compositor::snapshot(&self.client).await?;
            self.changed.notify_one();
            &refreshed
        } else {
            snapshot
        };
        let mut changed = false;
        for monitor in &snapshot.monitors {
            let Some(active) = monitor.active_workspace.id.filter(|id| *id > 0) else {
                continue;
            };
            let history = loaded
                .journal
                .history
                .entry(monitor.name.clone())
                .or_default();
            if history.first() != Some(&active) {
                history.retain(|id| *id != active);
                history.insert(0, active);
                history.truncate(128);
                changed = true;
            }
        }
        let ids: Vec<_> = loaded.journal.entries.keys().copied().collect();
        for id in ids {
            // Empty landing pads migrate too (for example after output removal).
            // Keep their identity while following observed compositor ownership.
            if let Some(live) = snapshot.workspaces.iter().find(|w| w.id == Some(id)) {
                let entry = loaded.journal.entries.get_mut(&id).unwrap();
                if live.monitor != entry.monitor {
                    entry.monitor = live.monitor.clone();
                    changed = true;
                }
            }
            let entry = loaded.journal.entries[&id].clone();
            if let Some(window) = snapshot
                .windows
                .iter()
                .filter(|w| w.mapped && w.workspace.id == Some(id))
                .min_by_key(|w| &w.address)
            {
                loaded.empty_since.remove(&id);
                let e = loaded.journal.entries.get_mut(&id).unwrap();
                if !e.occupied {
                    e.occupied = true;
                    changed = true;
                }
                if e.cue_empty || !e.cue_assigned {
                    let icon =
                        crate::desktop_identity::window_icon(&window.class, &window.initial_class);
                    let origin = if window.class.is_empty() {
                        &window.initial_class
                    } else {
                        &window.class
                    };
                    let origin = if origin.is_empty()
                        || origin.len() > 256
                        || origin.chars().any(char::is_control)
                    {
                        "Workspace"
                    } else {
                        origin
                    };
                    e.icon = icon.unwrap_or_else(|| "workspace".into());
                    e.origin = origin.into();
                    e.cue_assigned = true;
                    e.cue_empty = false;
                    changed = true;
                }
                continue;
            }
            if !entry.cue_empty {
                loaded.journal.entries.get_mut(&id).unwrap().cue_empty = true;
                changed = true;
            }
            if loaded.journal.leased(id) || loaded.journal.closing(id) {
                if !loaded.journal.closing(id)
                    && !entry.occupied
                    && !entry.landing_pad
                    && snapshot
                        .monitors
                        .iter()
                        .any(|m| m.active_workspace.id == Some(id))
                {
                    loaded.journal.entries.get_mut(&id).unwrap().landing_pad = true;
                    changed = true;
                }
                continue;
            }
            let active = snapshot
                .monitors
                .iter()
                .find(|m| m.active_workspace.id == Some(id));
            // Hyprland may discard an empty, inactive compositor workspace.
            // Keep its logical destination so focus/move can materialize it again.
            if entry.keep_until_closed {
                continue;
            }
            if entry.landing_pad && !entry.occupied && active.is_some() {
                continue;
            }
            let explicitly_closed = loaded
                .journal
                .closes
                .values()
                .any(|close| close.request.context.workspace_id == id);
            if entry.occupied && !explicitly_closed {
                let since = loaded.empty_since.entry(id).or_insert_with(|| {
                    self.wake_after_grace();
                    tokio::time::Instant::now()
                });
                if since.elapsed() < handoff::GRACE
                    || loaded
                        .journal
                        .handoffs
                        .values()
                        .any(|h| h.watching && h.window.workspace_id == id)
                {
                    continue;
                }
            }
            if let Some(monitor) = active {
                let fallback = presentation::fallback(&loaded.journal, snapshot, &monitor.name, id);
                let destination = match fallback {
                    Some(id) => id,
                    None => {
                        let next = loaded
                            .journal
                            .allocate(snapshot.workspaces.iter().filter_map(|w| w.id))?;
                        loaded.journal.entries.insert(
                            next,
                            Entry {
                                id: next,
                                generation: crate::state::publication::Publication::new().epoch,
                                monitor: monitor.name.clone(),
                                occupied: false,
                                landing_pad: true,
                                keep_until_closed: false,
                                origin: "Workspace".into(),
                                icon: "workspace".into(),
                                cue_assigned: false,
                                cue_empty: true,
                            },
                        );
                        loaded.save().await?;
                        next
                    }
                };
                compositor::execute(
                    &self.client,
                    &compositor::focus_local(&monitor.name, id, destination, false)?,
                )
                .await?;
                let observed = compositor::snapshot(&self.client).await?;
                ensure!(
                    observed
                        .monitors
                        .iter()
                        .any(|m| m.name == monitor.name
                            && m.active_workspace.id == Some(destination)),
                    "fallback focus was not observed"
                );
            }
            loaded.journal.entries.remove(&id);
            loaded.empty_since.remove(&id);
            changed = true;
        }
        if changed {
            loaded.save().await?;
        }
        Ok(())
    }

    pub async fn project(&self, state: &mut crate::model::WorkspaceState) {
        // Cache refresh runs outside lifecycle serialization; icon lookup is decorative.
        let _ = crate::desktop_identity::refresh().await;
        let result = async {
            let mut guard = self.loaded.lock().await;
            let snapshot = compositor::snapshot(&self.client).await?;
            let loaded = self.initialize(&mut guard, &snapshot).await?;
            self.cleanup(loaded, &snapshot).await?;
            let mut view = presentation::view(&loaded.journal, state);
            view.available &= enabled();
            Ok::<_, anyhow::Error>(view)
        }
        .await;
        state.lifecycle = match result {
            Ok(view) => view,
            Err(error) => {
                let mut view = presentation::view(&Journal::empty(String::new()), state);
                view.available = false;
                view.error = Some(error.to_string());
                view
            }
        };
    }
}

fn validate_context(
    snapshot: &compositor::Snapshot,
    context: &Context,
    active: bool,
) -> Result<()> {
    ensure!(
        context.session == snapshot.session && context.workspace_id > 0,
        "stale or unsupported workspace context"
    );
    let monitor = snapshot
        .monitors
        .iter()
        .find(|m| m.name == context.monitor)
        .context("monitor disappeared")?;
    ensure!(
        !active || monitor.active_workspace.id == Some(context.workspace_id),
        "workspace context changed"
    );
    Ok(())
}
