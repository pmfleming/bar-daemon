use super::*;
use serde_json::json;
use std::cell::{Cell, RefCell};

#[derive(Default)]
struct Fake {
    values: RefCell<Values>,
    calls: RefCell<Vec<Values>>,
    reject: Cell<bool>,
    ignore: Cell<bool>,
    ineligible: Cell<bool>,
    resume: Option<StateStore>,
    obstruct_save: Option<std::path::PathBuf>,
}
impl Backend for Fake {
    async fn eligible(&self) -> Result<()> {
        if self.ineligible.get() {
            bail!("inactive session");
        }
        if let Some(store) = &self.resume {
            store.record_resume().await;
        }
        Ok(())
    }
    async fn values(&self) -> Result<Values> {
        Ok(self.values.borrow().clone())
    }
    async fn configure(&self, values: &Values) -> Result<()> {
        self.calls.borrow_mut().push(values.clone());
        if self.reject.get() {
            bail!("compositor rejected change");
        }
        if !self.ignore.get() {
            self.values.borrow_mut().extend(values.clone());
        }
        if let Some(path) = &self.obstruct_save {
            std::fs::create_dir_all(path)?;
        }
        Ok(())
    }
}
fn values(value: Value) -> Values {
    serde_json::from_value(value).unwrap()
}
fn fake() -> Fake {
    Fake {
        values: RefCell::new(values(json!({"input:follow_mouse": 1,
        "misc:mouse_move_focuses_monitor": true, "cursor:no_warps": false}))),
        ..Default::default()
    }
}
#[test]
fn closed_schema_rejects_unknown_keys_commands_types_and_ranges() {
    for input in [
        json!({"exec": "anything"}),
        json!({"input:follow_mouse\"] = 1 }); evil()": 1}),
        json!({"input:follow_mouse": true}),
        json!({"input:follow_mouse": 4}),
        json!({"input:follow_mouse": -1}),
        json!({"input:follow_mouse": 1.5}),
        json!({"cursor:no_warps": 1}),
        json!({"cursor:no_warps": "false"}),
        json!({"input:follow_mouse_threshold": -0.1}),
        json!({"input:follow_mouse_threshold": 1001}),
        json!({"input:follow_mouse_shrink": 301}),
    ] {
        assert!(command(&values(input)).is_err());
    }
    assert!(command(&Values::new()).is_err());
    for (key, kind) in OPTIONS {
        let value = match kind {
            Bool => json!(true),
            Int(max) => json!(max),
            Float(max) => json!(max),
        };
        assert!(command(&BTreeMap::from([((*key).into(), value)])).is_ok());
    }
    assert_eq!(
        command(&values(json!({"cursor:no_warps": true}))).unwrap(),
        "eval hl.config({ [\"cursor.no_warps\"] = true })"
    );
    assert!(serde_json::from_value::<Patch>(json!({"values": {}, "command": "exec"})).is_err());
    assert!(serde_json::from_value::<Empty>(json!({"values": {}})).is_err());
    assert!(same("input:follow_mouse_threshold", &json!(1.0), &json!(1)));
}
#[test]
fn reads_observed_values_and_omits_unsupported_options_without_defaults() {
    let reply = OPTIONS
        .iter()
        .map(|(key, kind)| {
            if *key == "input:follow_mouse_shrink" {
                return "no such option".into();
            }
            match kind {
                Bool => json!({"option": key, "bool": false}),
                Int(_) => json!({"option": key, "int": 0}),
                Float(_) => json!({"option": key, "float": 0.5}),
            }
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n\n\n");
    let result = parse(&reply).unwrap();
    assert_eq!(result.len(), OPTIONS.len() - 1);
    assert!(!result.contains_key("input:follow_mouse_shrink"));
    assert_eq!(result["cursor:no_warps"], false);
    assert_eq!(result["input:follow_mouse_threshold"], 0.5);
    let legacy = reply.replace("\"bool\":false", "\"int\":0");
    assert_eq!(parse(&legacy).unwrap(), result);
    assert!(parse("ok").is_err());
    assert!(parse(&vec!["no such option"; OPTIONS.len()].join("\n\n\n")).is_err());
}
#[tokio::test]
async fn sparse_preferences_are_verified_persistent_reapplied_and_restorable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("focus.json");
    let store = StateStore::default();
    let backend = fake();
    let original = backend.values.borrow().clone();
    let observed = reconcile(&backend, &store, &path).await.unwrap();
    assert_eq!(observed.values, original);
    assert!(
        backend.calls.borrow().is_empty(),
        "opening settings is read-only"
    );
    let patch = values(json!({"input:follow_mouse": 0, "misc:mouse_move_focuses_monitor": false}));
    let result = change(&backend, Some(patch.clone()), &store, &path)
        .await
        .unwrap();
    assert_eq!(result.saved, patch);
    assert_eq!(
        result.values["cursor:no_warps"], false,
        "unowned settings are untouched"
    );
    reconcile(&backend, &store, &path).await.unwrap();
    assert_eq!(
        backend.calls.borrow().len(),
        1,
        "working values are not rewritten"
    );
    *backend.values.borrow_mut() = original.clone(); // compositor config reload / restart
    reconcile(&backend, &store, &path).await.unwrap();
    assert_eq!(backend.calls.borrow().len(), 2);
    change(
        &backend,
        Some(values(json!({"input:follow_mouse": 2}))),
        &store,
        &path,
    )
    .await
    .unwrap();
    let reset = change(&backend, None, &store, &path).await.unwrap();
    assert_eq!(
        reset.values, original,
        "reset restores the values before the first override"
    );
    assert!(reset.saved.is_empty());
    let saved: Document = load_json_or_default(&path, "test").await.unwrap();
    assert_eq!(saved, Document::default());
}
#[tokio::test]
async fn invalid_unsupported_unacknowledged_and_ineligible_changes_are_not_saved() {
    for failure in [
        "invalid",
        "unsupported",
        "rejected",
        "unacknowledged",
        "session",
        "sleep",
        "resume",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("focus.json");
        let store = StateStore::default();
        let mut backend = fake();
        let original = backend.values.borrow().clone();
        let mut patch = values(json!({"input:follow_mouse": 0}));
        match failure {
            "invalid" => patch = values(json!({"exec": 1})),
            "unsupported" => patch = values(json!({"input:focus_on_close": 1})),
            "rejected" => backend.reject.set(true),
            "unacknowledged" => backend.ignore.set(true),
            "session" => backend.ineligible.set(true),
            "sleep" => store.record_sleep_preparation(true).await,
            "resume" => backend.resume = Some(store.clone()),
            _ => unreachable!(),
        }
        assert!(
            change(&backend, Some(patch), &store, &path).await.is_err(),
            "{failure}"
        );
        assert!(!path.exists(), "{failure} must not persist");
        assert_eq!(*backend.values.borrow(), original);
    }
}
#[tokio::test]
async fn failed_persistence_rolls_back_compositor_and_unsupported_saved_options_can_be_reset() {
    let dir = tempfile::tempdir().unwrap();
    let mut backend = fake();
    let store = StateStore::default();
    let original = backend.values.borrow().clone();
    // Obstruct the atomic rename only after the compositor has changed, so this
    // exercises rollback on persistence failure rather than a failed load.
    let blocker = dir.path().join("blocker");
    backend.obstruct_save = Some(blocker.clone());
    assert!(
        change(
            &backend,
            Some(values(json!({"input:follow_mouse": 0}))),
            &store,
            &blocker
        )
        .await
        .is_err()
    );
    assert_eq!(*backend.values.borrow(), original);
    assert_eq!(
        backend.calls.borrow().len(),
        2,
        "apply followed by rollback"
    );
    backend.calls.borrow_mut().clear();
    backend.obstruct_save = None;
    let path = dir.path().join("focus.json");
    save_json_atomic(
        &path,
        &Document {
            saved: values(json!({"input:focus_on_close": 2})),
            previous: values(json!({"input:focus_on_close": 0})),
        },
    )
    .await
    .unwrap();
    let state = reconcile(&backend, &store, &path).await.unwrap();
    assert!(state.error.is_some());
    assert!(!state.values.contains_key("input:focus_on_close"));
    assert!(backend.calls.borrow().is_empty());
    assert!(
        change(&backend, None, &store, &path)
            .await
            .unwrap()
            .saved
            .is_empty()
    );
}
