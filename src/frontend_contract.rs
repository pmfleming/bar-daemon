//! The frontend checks byte-independent JSON parity with this producer fixture.
//! Decode with production DTOs so a changed field type cannot leave a green jq check.
use crate::activity::notifications::{
    center::{AppSummary, Preview},
    history::HistoryPage,
};
use crate::{model::BarSnapshot, protocol};
use serde_json::{Value, json};
fn method<'a>(fixture: &'a Value, name: &str) -> &'a Value {
    fixture["registry"]["methods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap()
}
#[test]
fn frontend_contract_snapshot_and_notification_types() {
    let f = protocol::contract_fixture().unwrap();
    assert_eq!(f["protocol"], "bar-api");
    assert_eq!(f["version"], 2);
    let snapshot: BarSnapshot = serde_json::from_value(f["snapshot"].clone()).unwrap();
    assert!(snapshot.compositor.available);
    assert_eq!(snapshot.compositor.revision, 1);
    assert_eq!(snapshot.compositor.animations_enabled, Some(false));
    let s = &f["snapshot"]; // semantic witnesses complement the typed whole-snapshot decode.
    assert_eq!(s["activity"]["lunar"]["approximate"], true);
    assert_eq!(s["activity"]["lunar"]["time_basis"], "utc");
    assert_eq!(s["activity"]["weather"]["solar_noon"]["approximate"], true);
    assert_eq!(
        s["activity"]["weather"]["solar_noon"]["timezone"],
        s["activity"]["weather"]["timezone"]
    );
    let player = &s["media"]["players"][0];
    assert_eq!(player["control_mode"], "automatic");
    assert_eq!(player["content_type"], "music");
    assert_eq!(player["content_type_source"], "url");
    assert_eq!(player["metadata_sources"], json!({}));
    assert!(player.get("owner").is_none());
    assert_eq!(
        player["source"],
        json!({"url":"spotify:track:example","service":"spotify"})
    );
    let display = &s["display_policy"];
    assert_eq!(display["docking_state"], "settled");
    assert_eq!(display["outputs"][0]["internal"], true);
    assert_eq!(display["outputs"][1]["supported"], true);
    assert_eq!(display["outputs"][1]["current_mode"], "3840x2160@59.940Hz");
    assert_eq!(
        display["outputs"][1]["modes"][0],
        json!({"id":"3840x2160@59.940Hz","width":3840,"height":2160,"rate":59.94,"size":"3840x2160"})
    );
    let page: HistoryPage = serde_json::from_value(f["notification_page"].clone()).unwrap();
    assert_eq!(page.revision, "1");
    assert_eq!(page.query, "");
    assert_eq!(page.scope_limit, 5000);
    assert!(page.anchor_reached);
    assert!(page.next_cursor.is_none());
    assert!(page.records[0].history_id.is_none());
    assert_eq!(
        page.records[0].notification.id,
        snapshot.notification_active.notifications[0].id
    );
    assert!(!page.records[0].notification.toast_visible);
    assert!(!page.records[0].notification.dnd_bypass);
    let apps: Vec<AppSummary> =
        serde_json::from_value(f["notification_apps"]["apps"].clone()).unwrap();
    let overview: Vec<Preview> =
        serde_json::from_value(f["notification_app"]["overview"].clone()).unwrap();
    let entries: Vec<Preview> =
        serde_json::from_value(f["notification_app"]["entries"].clone()).unwrap();
    assert_eq!(f["notification_apps"]["view"], "apps");
    assert_eq!(f["notification_apps"]["total_apps"], 1);
    assert_eq!(f["notification_app"]["view"], "app");
    assert_eq!(f["notification_app"]["pages"], 1);
    assert_eq!(f["notification_app"]["page"], 1);
    assert!(overview.len() <= 3 && entries.len() <= 5);
    assert_eq!(
        f["notification_app"]["selected"]["notification"],
        f["notification_page"]["records"][0]["notification"]
    );
    let selected = &page.records[0].notification;
    assert_eq!(json!(apps[0].key), f["notification_app"]["app_key"]);
    assert_eq!(json!(apps[0].count), f["notification_app"]["count"]);
    assert_eq!(
        json!(apps[0].total_count),
        f["notification_app"]["total_count"]
    );
    assert_eq!(apps[0].latest.id, selected.id);
    for preview in std::iter::once(&apps[0].latest)
        .chain(overview.iter())
        .chain(entries.iter())
    {
        assert_eq!(preview.hints.desktop_entry, selected.hints.desktop_entry);
        assert_eq!(preview.hints.image_path, selected.hints.image_path);
        assert_eq!(preview.identity_icon, selected.identity_icon);
    }
}
#[test]
fn frontend_contract_registry_semantics_and_malformed_snapshot_rejection() {
    let f = protocol::contract_fixture().unwrap();
    assert!(
        f["registry"]["streams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "compositor.changed")
    );
    assert!(
        method(&f, "media.operation")["params"]
            .get("mode")
            .is_some()
    );
    assert!(method(&f, "displayLayout.preview")["params"]["outputs"][0]["mirror_of"].is_string());
    let policy = method(&f, "notifications.setAppPolicy");
    assert_eq!(policy["result"], "notifications");
    assert!(policy["params"]["app_key"].is_string());
    assert_eq!(
        policy["params"]["policy"],
        json!({"silent":true,"until_unix_ms":null,"group_similar":true,"bypass_dnd":false})
    );
    for (name, result, fields) in [
        (
            "notifications.prepareDelete",
            "delete_confirmation",
            vec!["app_key", "selected"],
        ),
        ("notifications.delete", "deleted", vec!["token", "cancel"]),
    ] {
        let m = method(&f, name);
        assert_eq!(m["result"], result);
        for field in fields {
            assert!(m["params"].get(field).is_some());
        }
    }
    for key in ["cursor", "anchor", "query"] {
        assert!(
            method(&f, "notifications.queryHistory")["params"]
                .get(key)
                .is_some()
        );
    }
    for key in ["view", "page", "selected", "app_anchor", "period_day"] {
        assert!(
            method(&f, "notifications.queryCenter")["params"]
                .get(key)
                .is_some()
        );
    }
    assert!(method(&f, "notifications.queryCenter")["params"]["period_groups"].is_boolean());
    assert_eq!(method(&f, "updates.getOverview")["params"], json!({}));
    assert_eq!(method(&f, "updates.getOverview")["result"], "overview");
    assert_eq!(method(&f, "updates.queryHistory")["result"], "history");
    assert_eq!(
        method(&f, "updates.queryHistory")["params"],
        json!({"query":"","cursor":null,"run_id":null,"limit":50,"lane":null,"outcome":null,"anchor_ids":[]})
    );
    for path in [
        "/audio/volume_percent",
        "/activity/weather/condition_code",
        "/battery/percentage",
        "/display_policy/outputs",
        "/timezone/utc_offset_seconds",
    ] {
        let mut bad = f["snapshot"].clone();
        *bad.pointer_mut(path).unwrap() = json!({"wrong":"type"});
        assert!(
            serde_json::from_value::<BarSnapshot>(bad).is_err(),
            "{path}"
        );
    }
}
