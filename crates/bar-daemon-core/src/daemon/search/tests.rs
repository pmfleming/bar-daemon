use super::*;
use shelllist_daemon_tokio::{
    BasicCorrelation, JsonlListener, JsonlSocketConfig, LocalJsonlService, SocketSession,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::{Duration, timeout},
};

struct Probe(Search);
impl LocalJsonlService for Probe {
    async fn call(
        &self,
        session: &SocketSession,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        Ok(self
            .0
            .call(method, params, &Owner::Socket(session.clone()))
            .await)
    }
    async fn subscribe(&self, _: &SocketSession, _: Vec<String>) -> anyhow::Result<Value> {
        anyhow::bail!("unused")
    }
    async fn cancel(&self, _: &SocketSession, _: &str) -> anyhow::Result<Value> {
        anyhow::bail!("unused")
    }
    async fn disconnected(&self, _: &SocketSession) {}
}
async fn call(peer: &mut BufReader<UnixStream>, method: &str, params: Value) -> Value {
    peer.get_mut()
        .write_all(
            format!(
                "{}\n",
                json!({"id":"search", "op":"call", "method":method, "params":params})
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
async fn ranking_is_revision_fenced_owner_scoped_bounded_and_reclaimed_on_disconnect() {
    timeout(Duration::from_secs(5), async {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let search = Search::default();
        let listener =
            JsonlListener::bind(&std::fs::canonicalize(root.path()).unwrap(), "bar-daemon")
                .await
                .unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(listener.serve(
            Arc::new(Probe(search.clone())),
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
        let catalog = json!({"owner":"same", "revision":1, "items":[
            {"key":"a", "title":"Café", "score":0.25}, {"key":"b", "title":"Beta", "score":0.5}]});
        assert_eq!(
            call(&mut a, "search.catalog", catalog.clone()).await["ok"],
            true
        );
        let query = json!({"owner":"same", "revision":1, "generation":9, "query":""});
        assert_eq!(
            call(&mut a, "search.query", query.clone()).await["data"]["keys"],
            json!(["b", "a"])
        );
        let mut cafe = query.clone();
        cafe["query"] = "cafe".into();
        assert_eq!(
            call(&mut a, "search.query", cafe).await["data"]["keys"],
            json!(["a"])
        );
        assert_eq!(
            call(&mut b, "search.query", query.clone()).await["error"]["code"],
            "catalog-not-found"
        );
        assert_eq!(
            call(&mut b, "search.release", json!({"owner":"same"})).await["ok"],
            true
        );
        assert_eq!(
            call(&mut a, "search.query", query.clone()).await["ok"],
            true
        );
        assert_eq!(
            call(&mut a, "search.catalog", catalog.clone()).await["ok"],
            true
        );
        let mut conflicting = catalog;
        conflicting["items"] = json!([]);
        assert_eq!(
            call(&mut a, "search.catalog", conflicting).await["error"]["code"],
            "stale-catalog"
        );
        let next = json!({"owner":"same", "revision":2, "items":[]});
        assert_eq!(call(&mut a, "search.catalog", next).await["ok"], true);
        assert_eq!(
            call(&mut a, "search.query", query).await["error"]["code"],
            "stale-catalog"
        );
        assert_eq!(
            call(
                &mut a,
                "search.query",
                json!({"owner":"same", "revision":2, "generation":10, "query":""})
            )
            .await["data"]["keys"],
            json!([])
        );
        assert_eq!(
            call(
                &mut a,
                "search.catalog",
                json!({"owner":"bad", "revision":1, "items":vec![json!({"key":"x"}); MAX_ITEMS+1]})
            )
            .await["error"]["code"],
            "validation-error"
        );
        assert_eq!(
            call(&mut a, "search.release", json!({"owner":"same"})).await["ok"],
            true
        );
        assert!(
            search
                .catalogs
                .lock()
                .unwrap()
                .clients
                .values()
                .all(HashMap::is_empty)
        );
        assert_eq!(
            call(
                &mut a,
                "search.catalog",
                json!({"owner":"live", "revision":1, "items":[]})
            )
            .await["ok"],
            true
        );
        drop(a);
        while !search.catalogs.lock().unwrap().clients.is_empty() {
            tokio::task::yield_now().await;
        }
        drop(b);
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn preparation_does_not_hold_store_lock_and_release_fences_its_result() {
    let bus = crate::battery::helper::authorization_tests::Bus::new();
    let connection = bus.connection().await;
    let owner = Owner::bus(
        &connection,
        connection.unique_name().unwrap().to_string(),
        shelllist_daemon_tokio::OwnerLossMonitor::new(connection.clone()),
    );
    let search = Search::default();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    *search.preparation_barrier.lock().unwrap() = Some(barrier.clone());
    let preparing = {
        let search = search.clone();
        let owner = owner.clone();
        tokio::spawn(async move {
            search
                .call(
                    "search.catalog",
                    json!({"owner":"pending","revision":1,"items":[{"key":"one","title":"One"}]}),
                    &owner,
                )
                .await
        })
    };
    timeout(Duration::from_secs(2), async {
        while search.catalogs.lock().unwrap().pending.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let released = timeout(
        Duration::from_secs(2),
        search.call("search.release", json!({"owner":"pending"}), &owner),
    )
    .await
    .unwrap();
    assert_eq!(released["ok"], true);
    barrier.wait();
    assert_eq!(
        preparing.await.unwrap()["error"]["code"],
        "catalog-released"
    );
    assert!(search.catalogs.lock().unwrap().clients[owner.name()].is_empty());
}
