use super::super::advanced::{Advanced, Hardware};
use super::*;

fn available(f: &Fixture) {
    let mut outputs = f.backend.outputs.borrow_mut();
    let o = &mut outputs[0];
    o.vrr = Some(false);
    o.current_format = Some("XRGB8888".into());
    o.color_management_preset = Some("srgb".into());
    o.sdr_brightness = Some(1.0);
    o.sdr_saturation = Some(1.0);
    o.hardware_details = Hardware {
        hdr: Some(true),
        bt2020: Some(true),
        chroma: Some(true),
        vrr_capable: Some(true),
    };
}

#[tokio::test]
async fn unobserved_automatic_profile_restores_and_retains_a_durable_failure() {
    use crate::display_policy::profiles::Request;
    let mut f = Fixture::new();
    available(&f);
    let mut doc = super::super::profile_action_at(
        &f.backend,
        Request::Capture { name: "VRR".into() },
        &f.store,
        &f.path,
        &mut f.runtime,
    )
    .await
    .unwrap();
    doc.profiles.items[0].layout.outputs[0].advanced.vrr = Some(true);
    doc.profiles.automatic = true;
    crate::paths::save_json_atomic(&f.path, &doc).await.unwrap();
    let start = Instant::now();
    f.tick(super::super::now(), start).await.unwrap();
    f.tick(super::super::now(), start + Duration::from_secs(6))
        .await
        .unwrap();
    let trial = f.document().await.trial.unwrap();
    assert!(trial.automatic);
    f.backend.outputs.borrow_mut()[0].vrr = Some(false);
    assert!(
        f.tick(trial.expires_at, start + Duration::from_secs(30))
            .await
            .is_err()
    );
    let doc = f.document().await;
    assert!(doc.trial.is_none());
    assert!(doc.profiles.error.is_some());
    assert!(doc.profiles.attempted.is_some());
    assert_eq!(f.backend.outputs.borrow()[0].vrr, Some(false));
}

#[tokio::test]
async fn rollback_journal_survives_an_unobserved_restoration() {
    let mut f = Fixture::new();
    available(&f);
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].advanced.vrr = Some(true);
    let id = f.preview(proposed).await.unwrap().trial.unwrap().id;
    f.backend.ignore_advanced.set(true);
    assert!(f.finish(&id, false).await.is_err());
    assert!(f.document().await.trial.is_some());
    f.backend.ignore_advanced.set(false);
    f.finish(&id, false).await.unwrap();
    assert!(f.document().await.trial.is_none());
    assert_eq!(f.backend.outputs.borrow()[0].vrr, Some(false));
}

#[tokio::test]
async fn unknown_and_unsupported_advanced_settings_never_mutate() {
    let mut f = Fixture::new();
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].advanced.vrr = Some(true);
    assert!(f.preview(proposed).await.is_err());
    assert!(!f.path.exists());
    available(&f);
    f.backend.outputs.borrow_mut()[0].hardware_details.hdr = Some(false);
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].advanced.color = Some("hdr".into());
    assert!(f.preview(proposed).await.is_err());
    assert!(!f.path.exists());
    assert!(f.backend.calls.borrow().is_empty());
}

#[tokio::test]
async fn advanced_readback_rejects_silent_fallback_and_restart_restores_previous_state() {
    let mut f = Fixture::new();
    available(&f);
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].advanced = Advanced {
        vrr: Some(true),
        bit_depth: Some(10),
        color: Some("hdr".into()),
        sdr_brightness: Some(1.4),
        sdr_saturation: Some(0.9),
    };
    let doc = f.preview(proposed).await.unwrap();
    assert_eq!(f.backend.outputs.borrow()[0].vrr, Some(true));
    // Model a driver silently keeping 8-bit while accepting the command.
    f.backend.outputs.borrow_mut()[0].current_format = Some("XRGB8888".into());
    assert!(f.finish(&doc.trial.unwrap().id, true).await.is_err());
    assert!(f.document().await.trial.is_some());
    f.runtime = Runtime::default();
    let restored = f.tick(super::super::now(), Instant::now()).await.unwrap().0;
    assert!(restored.trial.is_none());
    let actual = Advanced::observed(&f.backend.outputs.borrow()[0]);
    assert_eq!(actual.vrr, Some(false));
    assert_eq!(actual.bit_depth, Some(8));
    assert_eq!(actual.color.as_deref(), Some("srgb"));
    assert_eq!(actual.sdr_brightness, Some(1.0));
    assert_eq!(actual.sdr_saturation, Some(1.0));
}

#[tokio::test]
async fn geometry_only_preview_and_revert_leave_advanced_settings_unmanaged() {
    let mut f = Fixture::new();
    available(&f);
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].advanced = Advanced::default();
    proposed.outputs[0].scale = 2.0;
    let doc = f.preview(proposed).await.unwrap();
    f.finish(&doc.trial.unwrap().id, false).await.unwrap();
    assert!(
        f.backend
            .calls
            .borrow()
            .iter()
            .all(|c| !c.contains("vrr =") && !c.contains("bitdepth =") && !c.contains("cm ="))
    );
}
