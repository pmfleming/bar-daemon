//! Real helper + fake polkit on a private bus, writing only a temporary BAT0.
use super::*;
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    time::Duration,
};

pub(crate) struct Bus {
    process: std::process::Child,
    address: String,
}
impl Bus {
    pub(crate) fn new() -> Self {
        let mut process = Command::new("dbus-daemon")
            .args([
                "--config-file",
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/authorization-session.conf"
                ),
                "--nofork",
                "--print-address=1",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            process,
            address: address.trim().into(),
        }
    }
    pub(crate) async fn connection(&self) -> Connection {
        connection::Builder::address(self.address.as_str())
            .unwrap()
            .method_timeout(super::super::authorization::HELPER_BUDGET)
            .build()
            .await
            .unwrap()
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[derive(Default)]
struct PolicyState {
    flags: AtomicU32,
    checks: AtomicUsize,
    cancellations: AtomicUsize,
    slow_noninteractive: AtomicBool,
}
struct Policy(Arc<PolicyState>);
#[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl Policy {
    async fn check_authorization(
        &self,
        _subject: (String, HashMap<String, zvariant::OwnedValue>),
        _action: &str,
        _details: HashMap<String, String>,
        flags: u32,
        cancellation_id: &str,
    ) -> (bool, bool, HashMap<String, String>) {
        assert!(!cancellation_id.is_empty());
        self.0.flags.store(flags, Ordering::SeqCst);
        self.0.checks.fetch_add(1, Ordering::SeqCst);
        if flags == 1 || self.0.slow_noninteractive.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(3200)).await;
        }
        (flags == 1, flags == 0, HashMap::new())
    }
    async fn cancel_check_authorization(&self, id: &str) {
        assert!(!id.is_empty());
        self.0.cancellations.fetch_add(1, Ordering::SeqCst);
    }
}

struct ApplyThenLoseReply {
    writer: ThresholdWriter,
    effects: Arc<AtomicUsize>,
}
#[zbus::interface(name = "org.laufan.BarBatteryHelper1")]
impl ApplyThenLoseReply {
    async fn set_thresholds(&self, battery: &str, start: u8, end: u8) -> (u8, u8, bool) {
        self.effects.fetch_add(1, Ordering::SeqCst);
        self.writer.set_thresholds(battery, start, end).unwrap();
        std::future::pending().await
    }
}

#[tokio::test]
async fn helper_applied_then_lost_reply_is_unknown_and_readback_does_not_retry() -> Result<()> {
    let bus = Bus::new();
    let (_directory, writer) = super::tests::writer(75, 80);
    let effects = Arc::new(AtomicUsize::new(0));
    let service = bus.connection().await;
    service
        .object_server()
        .at(
            OBJECT_PATH,
            ApplyThenLoseReply {
                writer: writer.clone(),
                effects: effects.clone(),
            },
        )
        .await?;
    service.request_name(BUS_NAME).await?;
    for disconnect in [false, true] {
        let caller = connection::Builder::address(bus.address.as_str())?
            .method_timeout(Duration::from_millis(100))
            .build()
            .await?;
        let closer = caller.clone();
        let count = effects.load(Ordering::SeqCst);
        let write = dispatch_write::<_, (u8, u8, bool)>(
            caller,
            "SetThresholds",
            &("BAT0", 60u8, 80u8),
            None,
        );
        let cut = async {
            while effects.load(Ordering::SeqCst) == count {
                tokio::task::yield_now().await;
            }
            if disconnect {
                closer.close().await.unwrap();
            }
        };
        let (result, ()) = tokio::join!(write, cut);
        assert!(super::super::authorization::is_unknown(
            &result.unwrap_err()
        ));
        assert_eq!(writer.get_thresholds("BAT0")?, (60, 80));
        assert_eq!(effects.load(Ordering::SeqCst), count + 1);
    }
    Ok(())
}

#[tokio::test]
async fn slow_interactive_authorization_and_owner_loss_never_touch_live_policy() -> Result<()> {
    let bus = Bus::new();
    let policy_state = Arc::new(PolicyState::default());
    let policy = bus.connection().await;
    policy
        .object_server()
        .at(POLKIT_PATH, Policy(policy_state.clone()))
        .await?;
    policy.request_name(POLKIT_BUS).await?;
    let (directory, writer) = super::tests::writer(75, 80);
    let service = bus.connection().await;
    service
        .object_server()
        .at(
            OBJECT_PATH,
            BatteryHelper {
                writer: writer.clone(),
            },
        )
        .await?;
    service.request_name(BUS_NAME).await?;
    let caller = bus.connection().await;
    let proxy = zbus::Proxy::new(&caller, BUS_NAME, OBJECT_PATH, INTERFACE).await?;
    let start = std::time::Instant::now();
    let result: (u8, u8, bool) = proxy.call("SetThresholds", &("BAT0", 60u8, 80u8)).await?;
    assert!(start.elapsed() > Duration::from_secs(3));
    assert_eq!(result, (60, 80, true));
    assert_eq!(policy_state.flags.load(Ordering::SeqCst), 1);
    let denied = proxy
        .call::<_, _, (u8, u8, bool)>("SetThresholdsNonInteractive", &("BAT0", 65u8, 80u8))
        .await;
    assert!(denied.is_err());
    assert_eq!(policy_state.flags.load(Ordering::SeqCst), 0);
    assert_eq!(writer.get_thresholds("BAT0")?, (60, 80));
    let cancel_caller = bus.connection().await;
    let task_connection = cancel_caller.clone();
    let request = tokio::spawn(async move {
        let proxy = zbus::Proxy::new(&task_connection, BUS_NAME, OBJECT_PATH, INTERFACE)
            .await
            .unwrap();
        proxy
            .call::<_, _, (u8, u8, bool)>("SetThresholds", &("BAT0", 70u8, 80u8))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while policy_state.checks.load(Ordering::SeqCst) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    cancel_caller.close().await?;
    tokio::time::timeout(Duration::from_secs(2), async {
        while policy_state.cancellations.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(3300)).await;
    assert_eq!(
        writer.get_thresholds("BAT0")?,
        (60, 80),
        "late authorization did not write after owner loss"
    );
    request.abort();
    policy_state
        .slow_noninteractive
        .store(true, Ordering::SeqCst);
    let expired = proxy
        .call::<_, _, (u8, u8, bool)>("SetThresholdsNonInteractive", &("BAT0", 65u8, 80u8))
        .await
        .unwrap_err();
    assert!(expired.to_string().contains("expired"));
    assert_eq!(policy_state.cancellations.load(Ordering::SeqCst), 2);
    assert_eq!(
        writer.get_thresholds("BAT0")?,
        (60, 80),
        "authorization deadline did not write"
    );
    assert!(directory.path().join("BAT0").is_dir());
    Ok(())
}
