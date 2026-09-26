use super::*;
use crate::display_policy::layout::{apply, apply_setting};

fn docked() -> Fixture {
    let f = Fixture::new();
    f.backend.outputs.borrow_mut().push(Output {
        name: "DP-1".into(),
        id: 1,
        width: 2560,
        height: 1440,
        scale: 1.0,
        refresh_rate: 75.0,
        x: 1536,
        available_modes: vec!["2560x1440@75.00Hz".into()],
        ..Default::default()
    });
    f
}
fn mirror(f: &Fixture) -> Layout {
    let mut layout = Layout::observed(&f.backend.outputs.borrow());
    layout.outputs[1].mirror_of = "eDP-1".into();
    layout
}

#[tokio::test]
async fn mirror_and_extend_are_verified_persistent_and_reversible() {
    let mut f = docked();
    let proposed = mirror(&f);
    let trial = f.preview(proposed.clone()).await.unwrap().trial.unwrap();
    assert_eq!(f.backend.outputs.borrow()[1].mirror_of, "0");
    assert_eq!(
        f.backend.outputs.borrow()[1].x,
        0,
        "mirror position follows source"
    );
    assert_eq!(
        f.backend.outputs.borrow()[1].width,
        2560,
        "each screen retains its own advertised mode"
    );
    // Acknowledged geometry is not enough: confirm also verifies the relationship.
    f.backend.outputs.borrow_mut()[1].mirror_of.clear();
    assert!(f.finish(&trial.id, true).await.is_err());
    f.backend.outputs.borrow_mut()[1].mirror_of = "0".into();
    let saved = f.finish(&trial.id, true).await.unwrap();
    assert_eq!(saved.saved, proposed);
    assert!(
        saved.manual_enablement,
        "docking must not undo a chosen mirror layout"
    );
    assert_eq!(f.document().await, saved);

    for rollback in [true, false] {
        let mut extended = Layout::observed(&f.backend.outputs.borrow());
        extended.outputs[1].mirror_of.clear();
        extended.outputs[1].x = 1600;
        let trial = f.preview(extended.clone()).await.unwrap().trial.unwrap();
        assert!(!f.backend.outputs.borrow()[1].mirrored());
        assert!(
            f.backend
                .calls
                .borrow()
                .iter()
                .any(|c| c.contains("mirror = \"\""))
        );
        let doc = f.finish(&trial.id, !rollback).await.unwrap();
        if rollback {
            assert_eq!(f.backend.outputs.borrow()[1].mirror_of, "0");
            assert_eq!(doc.saved, proposed);
        } else {
            assert_eq!(doc.saved, extended);
            assert!(!f.backend.outputs.borrow()[1].mirrored());
        }
    }
}

#[tokio::test]
async fn ignored_mirror_commands_cannot_be_confirmed_and_expiry_restores_extended() {
    let mut f = docked();
    f.backend.ignore_mirror.set(true);
    let trial = f.preview(mirror(&f)).await.unwrap().trial.unwrap();
    assert!(f.finish(&trial.id, true).await.is_err());
    f.backend.ignore_mirror.set(false);
    assert!(
        f.tick(trial.expires_at, Instant::now())
            .await
            .unwrap()
            .0
            .trial
            .is_none()
    );
    assert!(!f.backend.outputs.borrow()[1].mirrored());
    assert_eq!(f.backend.outputs.borrow()[1].x, 1536);
}

#[tokio::test]
async fn rejects_self_missing_disabled_and_chained_sources_before_mutation() {
    for invalid in ["self", "missing", "disabled", "cycle", "injection", "chain"] {
        let mut f = docked();
        let mut third = f.backend.outputs.borrow()[1].clone();
        third.name = "HDMI-A-1".into();
        third.id = 2;
        f.backend.outputs.borrow_mut().push(third);
        let mut proposed = mirror(&f);
        match invalid {
            "self" => proposed.outputs[1].mirror_of = "DP-1".into(),
            "missing" => proposed.outputs[1].mirror_of = "DP-99".into(),
            "disabled" => proposed.outputs[0].enabled = false,
            "cycle" => proposed.outputs[0].mirror_of = "DP-1".into(),
            "injection" => proposed.outputs[1].mirror_of = "eDP-1\" }); evil()".into(),
            "chain" => proposed.outputs[2].mirror_of = "DP-1".into(),
            _ => unreachable!(),
        }
        assert!(f.preview(proposed).await.is_err(), "{invalid}");
        assert!(f.backend.calls.borrow().is_empty());
        assert!(f.document().await.trial.is_none());
    }
}

#[tokio::test]
async fn source_handoffs_detach_children_before_converting_the_old_source() {
    for third_screen in [false, true] {
        let mut f = docked();
        let trial = f.preview(mirror(&f)).await.unwrap().trial.unwrap();
        f.finish(&trial.id, true).await.unwrap();
        if third_screen {
            let mut third = f.backend.outputs.borrow()[0].clone();
            third.name = "HDMI-A-1".into();
            third.id = 2;
            f.backend.outputs.borrow_mut().push(third);
        }
        let mut proposed = Layout::observed(&f.backend.outputs.borrow());
        proposed.outputs[0].mirror_of = if third_screen { "HDMI-A-1" } else { "DP-1" }.into();
        proposed.outputs[1].mirror_of = if third_screen { "HDMI-A-1" } else { "" }.into();
        f.backend.calls.borrow_mut().clear();
        let trial = f.preview(proposed).await.unwrap().trial.unwrap();
        {
            let calls = f.backend.calls.borrow();
            assert!(calls[0].contains("output = \"DP-1\""));
            assert!(calls[0].contains("mirror = \"\""), "detach old child first");
        }
        f.finish(&trial.id, true).await.unwrap();
    }
}

#[tokio::test]
async fn a_promoted_mirror_can_replace_its_disabled_source() {
    let mut f = docked();
    let trial = f.preview(mirror(&f)).await.unwrap().trial.unwrap();
    f.finish(&trial.id, true).await.unwrap();
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].enabled = false;
    proposed.outputs[1].mirror_of.clear();
    f.backend.calls.borrow_mut().clear();
    let trial = f.preview(proposed).await.unwrap().trial.unwrap();
    f.finish(&trial.id, true).await.unwrap();
    assert!(f.backend.outputs.borrow()[0].disabled);
    assert!(f.backend.outputs.borrow()[1].usable());
    assert!(f.backend.calls.borrow()[0].contains("output = \"DP-1\""));
    assert!(
        f.backend
            .calls
            .borrow()
            .last()
            .unwrap()
            .contains("disabled = true")
    );
}

#[tokio::test]
async fn mirrors_are_not_independent_fallbacks_and_sources_are_rechecked() {
    let f = docked();
    apply(&f.backend, &mirror(&f), &f.store).await.unwrap();
    f.backend.calls.borrow_mut().clear();
    let mut disable = Layout::observed(&f.backend.outputs.borrow()).outputs[0].clone();
    disable.enabled = false;
    assert!(
        apply_setting(&f.backend, &disable, &f.store, 0)
            .await
            .is_err()
    );
    assert!(f.backend.calls.borrow().is_empty());
    // A source disappearing or becoming disabled before the mirror IPC is unsafe.
    f.backend.outputs.borrow_mut()[1].mirror_of.clear();
    let requested = mirror(&f).outputs[1].clone();
    f.backend.outputs.borrow_mut()[0].disabled = true;
    assert!(
        apply_setting(&f.backend, &requested, &f.store, 0)
            .await
            .is_err()
    );
    assert!(f.backend.calls.borrow().is_empty());
}

#[tokio::test]
async fn source_unplug_promotes_surviving_mirrors_for_rollback_and_saved_layouts() {
    for saved_layout in [false, true] {
        let mut f = docked();
        let trial = f.preview(mirror(&f)).await.unwrap().trial.unwrap();
        f.finish(&trial.id, true).await.unwrap();
        if !saved_layout {
            let mut extended = Layout::observed(&f.backend.outputs.borrow());
            extended.outputs[1].mirror_of.clear();
            f.preview(extended).await.unwrap();
        }
        f.backend.outputs.borrow_mut().remove(0);
        let start = Instant::now();
        f.tick(0, start).await.unwrap();
        let doc = f.tick(5, start + Duration::from_secs(5)).await.unwrap().0;
        assert!(doc.trial.is_none());
        assert!(f.backend.outputs.borrow()[0].usable());
        assert_eq!(
            doc.saved.outputs[1].mirror_of, "eDP-1",
            "retain saved intent for reconnect"
        );
        let source = fake().outputs.borrow()[0].clone();
        f.backend.outputs.borrow_mut().push(source);
        f.tick(10, start + Duration::from_secs(10)).await.unwrap();
        f.tick(15, start + Duration::from_secs(15)).await.unwrap();
        assert_eq!(f.backend.outputs.borrow()[0].mirror_of, "0");
    }
}

#[test]
fn mirror_metadata_resolves_ids_and_old_layout_documents_stay_extended() {
    let f = docked();
    for reference in [
        serde_json::json!(0),
        serde_json::json!("0"),
        serde_json::json!("eDP-1"),
    ] {
        let mut value = serde_json::to_value(&f.backend.outputs.borrow()[1]).unwrap();
        value["mirrorOf"] = reference;
        f.backend.outputs.borrow_mut()[1] = serde_json::from_value(value).unwrap();
        assert_eq!(
            Layout::observed(&f.backend.outputs.borrow()).outputs[1].mirror_of,
            "eDP-1"
        );
        assert!(!f.backend.outputs.borrow()[1].usable());
    }
    for reference in [
        serde_json::json!("none"),
        serde_json::json!(-1),
        serde_json::Value::Null,
    ] {
        let mut value = serde_json::to_value(&f.backend.outputs.borrow()[1]).unwrap();
        value["mirrorOf"] = reference;
        f.backend.outputs.borrow_mut()[1] = serde_json::from_value(value).unwrap();
        assert!(f.backend.outputs.borrow()[1].usable());
    }
    let mut old =
        serde_json::to_value(&Layout::observed(&f.backend.outputs.borrow()).outputs[0]).unwrap();
    old.as_object_mut().unwrap().remove("mirror_of");
    assert!(
        serde_json::from_value::<Setting>(old)
            .unwrap()
            .mirror_of
            .is_empty()
    );
}
