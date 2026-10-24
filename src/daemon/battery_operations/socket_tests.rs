//! Socket ownership against the real admission/cancellation coordinator; fake
//! authorization work only. No battery helper, hardware or host D-Bus is used.
use super::*;
use shelllist_daemon_tokio::{
    BasicCorrelation, JsonlListener, JsonlSocketConfig, LocalJsonlService, SocketSession,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::{Notify, Semaphore},
    time::timeout,
};
#[derive(Clone)]
struct Probe {
    operations: Operations,
    started: Arc<Notify>,
    cancelled: Arc<Notify>,
    settled: Arc<Semaphore>,
}
impl LocalJsonlService for Probe {
    async fn call(
        &self,
        session: &SocketSession,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        if method != "start" {
            return Ok(self.operations.lookup(method, params, session.owner()));
        }
        let started = self.started.clone();
        let cancelled = self.cancelled.clone();
        let settled = self.settled.clone();
        Ok(self
            .operations
            .start(Owner::Socket(session.clone()), async move {
                let mut cancellation = authorization::cancellation().unwrap();
                started.notify_one();
                authorization::cancelled(&mut cancellation).await;
                cancelled.notify_one();
                settled.acquire().await.unwrap().forget();
                api::error("outcome-unknown", "private fixture reconciled cancellation")
            }))
    }
    async fn subscribe(&self, _: &SocketSession, _: Vec<String>) -> anyhow::Result<Value> {
        anyhow::bail!("unused")
    }
    async fn cancel(&self, session: &SocketSession, id: &str) -> anyhow::Result<Value> {
        Ok(self.operations.lookup(
            "battery.operation.cancel",
            json!({"operation_id":id}),
            session.owner(),
        ))
    }
    async fn disconnected(&self, _: &SocketSession) {}
}
async fn request(peer: &mut BufReader<UnixStream>, method: &str, params: Value) -> Value {
    peer.get_mut()
        .write_all(
            format!(
                "{}\n",
                json!({"id":"request", "op":"call", "method":method,"params":params})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut line = String::new();
    assert!(peer.read_line(&mut line).await.unwrap() > 0);
    serde_json::from_str::<Value>(&line).unwrap()["response"].clone()
}
#[tokio::test]
async fn socket_loss_requests_cancellation_but_holds_admission_until_reconciliation() {
    timeout(Duration::from_secs(5), async {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let probe = Arc::new(Probe {
            operations: Operations::default(),
            started: Arc::new(Notify::new()),
            cancelled: Arc::new(Notify::new()),
            settled: Arc::new(Semaphore::new(0)),
        });
        let listener =
            JsonlListener::bind(&std::fs::canonicalize(root.path()).unwrap(), "bar-daemon")
                .await
                .unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(listener.serve(
            probe.clone(),
            BasicCorrelation,
            JsonlSocketConfig::default(),
            async {
                let _ = stopped.await;
            },
        ));
        let mut a = BufReader::new(
            UnixStream::connect(root.path().join("bar-daemon.sock"))
                .await
                .unwrap(),
        );
        let mut b = BufReader::new(
            UnixStream::connect(root.path().join("bar-daemon.sock"))
                .await
                .unwrap(),
        );
        let accepted = request(&mut a, "start", json!({})).await;
        probe.started.notified().await;
        let id = accepted["data"]["operation"]["id"].as_str().unwrap();
        assert_eq!(
            request(
                &mut b,
                "battery.operation.status",
                json!({"operation_id":id})
            )
            .await["error"]["code"],
            "operation-not-found"
        );
        assert_eq!(
            request(
                &mut b,
                "battery.operation.cancel",
                json!({"operation_id":id})
            )
            .await["error"]["code"],
            "operation-not-found"
        );
        drop(a);
        probe.cancelled.notified().await;
        assert_eq!(
            request(&mut b, "start", json!({})).await["error"]["code"],
            "operation-active"
        );
        probe.settled.add_permits(1);
        while !probe.operations.0.lock().unwrap().entries.is_empty() {
            tokio::task::yield_now().await;
        }
        let next = request(&mut b, "start", json!({})).await;
        assert_eq!(next["ok"], true);
        assert_ne!(next["data"]["operation"]["id"], id);
        probe.started.notified().await;
        drop(b);
        probe.cancelled.notified().await;
        probe.settled.add_permits(1);
        while !probe.operations.0.lock().unwrap().entries.is_empty() {
            tokio::task::yield_now().await;
        }
        let _ = stop.send(());
        serving.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}
