//! Deterministic typed observations for offline protocol examples, not effect execution.
//! Baselines and trial IDs are opaque examples; real values are process/time scoped.
use super::{
    DisplayPolicyState, Output, Transition, focus,
    layout::{Document, Layout, Setting, Trial},
};
use serde_json::json;
use std::{collections::BTreeMap, time::Instant};

pub(crate) fn states() -> BTreeMap<String, DisplayPolicyState> {
    let initial = DisplayPolicyState {
        available: true,
        baseline: "baseline-initial".into(),
        status: "internal".into(),
        outputs: vec![Output {
            id: 0,
            name: "eDP-1".into(),
            width: 1920,
            height: 1200,
            scale: 1.25,
            refresh_rate: 60.0,
            available_modes: vec!["1920x1200@60.00Hz".into()],
            ..Default::default()
        }],
        focus: focus::State {
            available: true,
            values: BTreeMap::from([
                ("input:follow_mouse".into(), json!(1)),
                ("cursor:no_warps".into(), json!(false)),
            ]),
            saved: BTreeMap::from([("input:follow_mouse".into(), json!(1))]),
            error: None,
        },
        ..Default::default()
    };
    let previous = Layout {
        outputs: vec![Setting {
            name: "eDP-1".into(),
            mode: "1920x1200@60.00Hz".into(),
            x: 0,
            y: 0,
            scale: 1.25,
            transform: 0,
            enabled: true,
            mirror_of: String::new(),
        }],
    };
    let mut proposed = previous.clone();
    proposed.outputs[0].scale = 2.0;
    let mut preview = initial.clone();
    preview.baseline = "baseline-preview".into();
    preview.outputs[0].scale = 2.0;
    preview.layout.trial = Some(Trial {
        id: "trial-example".into(),
        expires_at: 2_147_483_647,
        proposed: proposed.clone(),
        previous,
    });
    let mut confirmed = preview.clone();
    confirmed.layout = Document {
        saved: proposed,
        ..Default::default()
    };
    let mut expired = preview.clone();
    expired.layout.trial.as_mut().unwrap().expires_at = 1;
    let mut rollback_failed = preview.clone();
    Transition::default().update(
        &mut rollback_failed,
        Err(anyhow::anyhow!("compositor unavailable").context("Layout rollback failed")),
        Instant::now(),
    );
    let mut policy = initial.clone();
    policy.policy.prefer_external = false;
    policy.baseline = "baseline-policy".into();
    Transition::default().update(&mut policy, Ok("pending"), Instant::now());
    let mut settled = policy.clone();
    Transition::default().update(&mut settled, Ok("internal"), Instant::now());
    let mut focused = initial.clone();
    focused
        .focus
        .values
        .insert("input:follow_mouse".into(), json!(0));
    focused
        .focus
        .saved
        .insert("input:follow_mouse".into(), json!(0));
    let mut reset = initial.clone();
    reset.focus.saved.clear();
    let mut stale = initial.clone();
    stale.baseline = "baseline-external-change".into();
    stale.outputs[0].scale = 1.5;
    BTreeMap::from([
        ("initial".into(), initial.clone()),
        ("preview".into(), preview),
        ("confirmed".into(), confirmed),
        ("reverted".into(), initial),
        ("expired".into(), expired),
        ("rollback-failed".into(), rollback_failed),
        ("policy".into(), policy),
        ("settled".into(), settled),
        ("focus".into(), focused),
        ("focusReset".into(), reset),
        ("stale".into(), stale),
    ])
}
