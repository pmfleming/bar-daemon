//! Presentation privacy is separate from user DND. Detection never changes DND
//! preferences and unknown observation never means a previously active share ended.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Sharing {
    pub available: bool,
    pub active: bool,
    pub prompt: bool,
    pub generation: u64,
    pub until_unix_ms: Option<u64>,
    pub until_sharing_ends: bool,
}
impl Sharing {
    pub(super) fn suppressed(&self, now: u64) -> bool {
        self.prompt
            || self.active && self.until_sharing_ends
            || self.until_unix_ms.is_some_and(|until| until > now)
    }
    pub(super) fn observe(&mut self, observed: Option<bool>) {
        self.available = observed.is_some();
        let Some(active) = observed else {
            return;
        };
        if active && !self.active {
            self.generation = self.generation.wrapping_add(1);
            self.prompt = true;
            self.until_sharing_ends = true;
        } else if !active {
            self.prompt = false;
            self.until_sharing_ends = false;
        }
        self.active = active;
    }
    pub(super) fn snooze(&mut self, generation: u64, minutes: u16, now: u64) -> Result<()> {
        ensure!(
            self.active && self.generation == generation,
            "Screen-sharing session changed"
        );
        ensure!(
            [0, 15, 60].contains(&minutes),
            "Choose 15 minutes, 1 hour or until sharing ends"
        );
        self.prompt = false;
        self.until_sharing_ends = minutes == 0;
        self.until_unix_ms = (minutes > 0).then(|| now.saturating_add(u64::from(minutes) * 60_000));
        Ok(())
    }
    pub(super) fn expire(&mut self, now: u64) {
        super::model::take_due(&mut self.until_unix_ms, now);
    }
}

fn screen_node(class: &str, role: &str, name: &str) -> bool {
    class == "Video/Source"
        && (role.eq_ignore_ascii_case("screen")
            || ["xdg-desktop-portal-hyprland", "xdg-desktop-portal-wlr"]
                .iter()
                .any(|prefix| name == *prefix || name.starts_with(&format!("{prefix}-"))))
}

pub(super) async fn monitor(engine: Arc<super::engine::NotificationEngine>) {
    let (sender, mut receiver) = watch::channel(None);
    if let Err(error) = std::thread::Builder::new()
        .name("notification-sharing".into())
        .spawn(move || {
            while !sender.is_closed() {
                if let Err(error) = observe_pipewire(&sender) {
                    tracing::warn!(%error, "Screen-sharing observation unavailable");
                }
                sender.send_replace(None);
                std::thread::sleep(Duration::from_secs(2));
            }
        })
    {
        tracing::warn!(%error, "Cannot start screen-sharing observer");
        return;
    }
    while receiver.changed().await.is_ok() {
        let observed = *receiver.borrow_and_update();
        engine.observe_sharing(observed).await;
    }
}

fn observe_pipewire(sender: &watch::Sender<Option<bool>>) -> Result<()> {
    use pipewire as pw;
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None).context("create sharing observer loop")?;
    let context =
        pw::context::ContextRc::new(&main_loop, None).context("create sharing observer context")?;
    let core = context
        .connect_rc(None)
        .context("connect sharing observer")?;
    let registry = core.get_registry_rc()?;
    let nodes = Rc::new(RefCell::new(BTreeSet::new()));
    let added = nodes.clone();
    let removed = nodes.clone();
    let add_sender = sender.clone();
    let remove_sender = sender.clone();
    let _registry = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != pw::types::ObjectType::Node {
                return;
            }
            let Some(props) = global.props else {
                return;
            };
            if screen_node(
                props.get("media.class").unwrap_or_default(),
                props.get("media.role").unwrap_or_default(),
                props.get("node.name").unwrap_or_default(),
            ) {
                added.borrow_mut().insert(global.id);
                add_sender.send_replace(Some(true));
            }
        })
        .global_remove(move |id| {
            if removed.borrow_mut().remove(&id) {
                remove_sender.send_replace(Some(!removed.borrow().is_empty()));
            }
        })
        .register();
    let sequence = core.sync(0)?;
    let ready = Rc::new(Cell::new(false));
    let done_ready = ready.clone();
    let done_sender = sender.clone();
    let error_loop = main_loop.clone();
    let _core = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == sequence {
                done_ready.set(true);
                done_sender.send_replace(Some(!nodes.borrow().is_empty()));
            }
        })
        .error(move |_, _, _, _| error_loop.quit())
        .register();
    let timer_loop = main_loop.clone();
    let timer_sender = sender.clone();
    let started = Instant::now();
    let timer = main_loop.loop_().add_timer(move |_| {
        if timer_sender.is_closed() || !ready.get() && started.elapsed() > Duration::from_secs(3) {
            timer_loop.quit();
        }
    });
    timer
        .update_timer(Some(Duration::from_secs(1)), Some(Duration::from_secs(1)))
        .into_result()?;
    main_loop.run();
    anyhow::bail!("PipeWire sharing observer disconnected")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sharing_snooze_is_session_fenced_and_does_not_treat_unknown_as_stopped() {
        let mut sharing = Sharing::default();
        sharing.observe(Some(true));
        assert!(sharing.prompt && sharing.suppressed(100));
        sharing.observe(None);
        assert!(!sharing.available && sharing.suppressed(100));
        assert!(sharing.snooze(0, 15, 100).is_err());
        sharing.snooze(1, 15, 100).unwrap();
        assert!(!sharing.prompt && sharing.suppressed(899_999));
        sharing.expire(900_100);
        assert!(!sharing.suppressed(900_100));
        sharing.observe(Some(false));
        sharing.observe(Some(true));
        assert_eq!(sharing.generation, 2);
        sharing.snooze(2, 0, 1_000_000).unwrap();
        assert!(sharing.suppressed(u64::MAX));
        sharing.observe(Some(false));
        assert!(!sharing.suppressed(u64::MAX));
    }
    #[test]
    fn screen_sources_exclude_webcams_and_audio() {
        assert!(screen_node("Video/Source", "Screen", "portal-video"));
        assert!(screen_node(
            "Video/Source",
            "",
            "xdg-desktop-portal-hyprland"
        ));
        assert!(!screen_node("Video/Source", "Camera", "v4l2-camera"));
        assert!(!screen_node("Audio/Source", "Screen", "monitor"));
        assert!(!screen_node("Video/Source", "", "unrelated-screen-app"));
    }
}
