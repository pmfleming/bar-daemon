//! Optional end-to-end test of the packaged native patch. Both D-Bus and
//! Wayland are private mocks; generated callbacks only write a temporary log.
use std::{
    io::{BufRead, BufReader},
    sync::{Arc, Mutex},
    time::Duration,
};
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource,
    protocol::wl_seat,
};

mod protocol {
    #[allow(clippy::single_component_path_imports)]
    use wayland_server;
    use wayland_server::protocol::*;
    pub mod __interfaces {
        use wayland_server::backend as wayland_backend;
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/ext-idle-notify-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("protocols/ext-idle-notify-v1.xml");
}
use protocol::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1 as Notification},
    ext_idle_notifier_v1::{self, ExtIdleNotifierV1 as Notifier},
};
type Listeners = Arc<Mutex<Vec<(u32, Notification)>>>;
struct Compositor(Listeners);
impl GlobalDispatch<Notifier, ()> for Compositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<Notifier>,
        _: &(),
        data: &mut DataInit<'_, Self>,
    ) {
        data.init(resource, ());
    }
}
impl GlobalDispatch<wl_seat::WlSeat, ()> for Compositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<wl_seat::WlSeat>,
        _: &(),
        data: &mut DataInit<'_, Self>,
    ) {
        data.init(resource, ());
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_seat::WlSeat,
        _: wl_seat::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}
impl Dispatch<Notifier, ()> for Compositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &Notifier,
        request: ext_idle_notifier_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let ext_idle_notifier_v1::Request::GetIdleNotification { id, timeout, .. }
        | ext_idle_notifier_v1::Request::GetInputIdleNotification { id, timeout, .. } = request
        {
            state.0.lock().unwrap().push((timeout, data.init(id, ())));
        }
    }
}
impl Dispatch<Notification, ()> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &Notification,
        _: ext_idle_notification_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Login;
#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl Login {
    fn get_session(&self, session: &str) -> zvariant::OwnedObjectPath {
        assert_eq!(session, "auto");
        "/org/freedesktop/login1/session/test".try_into().unwrap()
    }
    #[zbus(property)]
    fn block_inhibited(&self) -> &str {
        ""
    }
}
async fn state(proxy: &zbus::Proxy<'_>) -> (u32, String, u32, u64, bool) {
    let (state,): ((u32, String, u32, u64, bool),) = proxy.call("GetState", &()).await.unwrap();
    state
}
fn notification(listeners: &Listeners, timeout: u32) -> Notification {
    listeners
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(ms, n)| *ms == timeout && n.is_alive())
        .unwrap()
        .1
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "set HYPRIDLE_TEST_BIN to the patched native executable; uses private mock buses/display"]
async fn native_control_preserves_cookies_and_base_listeners_and_cancels_activity() {
    let executable = std::env::var("HYPRIDLE_TEST_BIN").expect("patched native binary is required");
    let dir = tempfile::tempdir().unwrap();
    let bus_config = dir.path().join("bus.conf");
    std::fs::write(
        &bus_config,
        include_str!("../test_support/dbus-session.conf"),
    )
    .unwrap();
    let mut bus = Child(
        std::process::Command::new("dbus-daemon")
            .arg(format!("--config-file={}", bus_config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut address = String::new();
    BufReader::new(bus.0.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let address = address.trim();
    let _login = zbus::connection::Builder::address(address)
        .unwrap()
        .name("org.freedesktop.login1")
        .unwrap()
        .serve_at("/org/freedesktop/login1", Login)
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = zbus::connection::Builder::address(address)
        .unwrap()
        .method_timeout(Duration::from_secs(3))
        .build()
        .await
        .unwrap();
    let socket = std::os::unix::net::UnixListener::bind(dir.path().join("wayland-test")).unwrap();
    socket.set_nonblocking(true).unwrap();
    let listeners: Listeners = Arc::default();
    let observed = listeners.clone();
    let mut display = Display::<Compositor>::new().unwrap();
    display
        .handle()
        .create_global::<Compositor, Notifier, _>(2, ());
    display
        .handle()
        .create_global::<Compositor, wl_seat::WlSeat, _>(7, ());
    let _compositor = Task(tokio::spawn(async move {
        let mut state = Compositor(observed);
        loop {
            if let Ok((socket, _)) = socket.accept() {
                display
                    .handle()
                    .insert_client(socket, Arc::new(()))
                    .unwrap();
            }
            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }));
    let config = dir.path().join("hypridle.conf");
    std::fs::write(&config, "general {\n inhibit_sleep = 0\n}\nlistener {\n timeout = 123\n on-timeout = true\n}\nlistener {\n timeout = 456\n on-timeout = true\n}\n").unwrap();
    let callback = dir.path().join("callback");
    std::fs::write(
        &callback,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            dir.path().join("calls").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&callback, std::fs::Permissions::from_mode(0o700)).unwrap();
    let ready = tokio::net::UnixDatagram::bind(dir.path().join("notify")).unwrap();
    let log_path = dir.path().join("hypridle.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut idle_process = Child(
        std::process::Command::new(executable)
            .arg("--config")
            .arg(config)
            .env("DBUS_SYSTEM_BUS_ADDRESS", address)
            .env("DBUS_SESSION_BUS_ADDRESS", address)
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path())
            .env("XDG_CONFIG_DIRS", dir.path())
            .env("XDG_CACHE_HOME", dir.path())
            .env("XDG_RUNTIME_DIR", dir.path())
            .env("WAYLAND_DISPLAY", "wayland-test")
            .env_remove("WAYLAND_SOCKET")
            .env("NOTIFY_SOCKET", dir.path().join("notify"))
            .env("BAR_DAEMON_IDLE_MANAGED", "1")
            .env("BAR_DAEMON_IDLE_GENERATION", "42-test")
            .env("BAR_DAEMON_IDLE_MINUTES", "30")
            .env("BAR_DAEMON_IDLE_COMMAND", callback)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let mut buf = [0; 256];
    match tokio::time::timeout(Duration::from_secs(5), ready.recv(&mut buf)).await {
        Ok(Ok(length)) => assert!(buf[..length].starts_with(b"READY=1\n")),
        result => panic!(
            "native idle did not become ready: {result:?}; exit={:?}\n{}",
            idle_process.0.try_wait(),
            std::fs::read_to_string(&log_path).unwrap()
        ),
    }
    let control = zbus::Proxy::new(
        &client,
        "org.laufan.Hypridle",
        "/org/laufan/Hypridle",
        "org.laufan.Hypridle1",
    )
    .await
    .unwrap();
    let saver = zbus::Proxy::new(
        &client,
        "org.freedesktop.ScreenSaver",
        "/org/freedesktop/ScreenSaver",
        "org.freedesktop.ScreenSaver",
    )
    .await
    .unwrap();
    let before = state(&control).await;
    let base1 = notification(&listeners, 123000);
    let base2 = notification(&listeners, 456000);
    let cookie: u32 = saver
        .call("Inhibit", &("test", "keep session awake"))
        .await
        .unwrap();
    let _: () = control
        .call("SetTimeout", &(before.1, 45u32))
        .await
        .unwrap();
    let changed = state(&control).await;
    assert_eq!(changed.0, before.0);
    assert_eq!(changed.2, 45);
    assert_eq!(notification(&listeners, 123000).id(), base1.id());
    assert_eq!(notification(&listeners, 456000).id(), base2.id());
    notification(&listeners, 45 * 60000).idled();
    assert!(
        !state(&control).await.4,
        "the application inhibitor must survive a timeout change"
    );
    assert!(!dir.path().join("calls").exists());
    let _: () = saver.call("UnInhibit", &(cookie,)).await.unwrap();
    state(&control).await;
    notification(&listeners, 45 * 60000).idled();
    let idle = state(&control).await;
    assert!(idle.4);
    let _: () = control
        .call("SetTimeout", &(idle.1.clone(), 45u32))
        .await
        .unwrap();
    assert_eq!(
        state(&control).await,
        idle,
        "an unchanged timeout must preserve its idle episode"
    );
    notification(&listeners, 123000).resumed();
    let resumed = state(&control).await;
    assert!(!resumed.4);
    assert_ne!(resumed.3, idle.3);
    let _: () = control
        .call("SetTimeout", &(resumed.1, 0u32))
        .await
        .unwrap();
    let never = state(&control).await;
    assert_eq!(never.2, 0);
    assert!(!never.4);
    assert!(
        listeners
            .lock()
            .unwrap()
            .iter()
            .filter(|(ms, _)| *ms == 45 * 60000)
            .all(|(_, n)| !n.is_alive())
    );
}
