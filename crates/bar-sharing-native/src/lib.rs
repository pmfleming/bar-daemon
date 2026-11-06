//! Native sharing observation. Notification state and privacy decisions stay in Core.
use anyhow::{Context, Result};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    rc::Rc,
    time::{Duration, Instant},
};
use tokio::sync::watch;
fn screen_node(class: &str, role: &str, name: &str) -> bool {
    class == "Video/Source"
        && (role.eq_ignore_ascii_case("screen")
            || ["xdg-desktop-portal-hyprland", "xdg-desktop-portal-wlr"]
                .iter()
                .any(|prefix| name == *prefix || name.starts_with(&format!("{prefix}-"))))
}

pub fn observe() -> watch::Receiver<Option<bool>> {
    let (sender, receiver) = watch::channel(None);
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
    }
    receiver
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
