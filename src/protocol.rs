use serde_json::{Value, json};

pub const NAME: &str = "bar-api";
pub const VERSION: u8 = 1;

pub mod stream {
    pub const ACTIVITY: &str = "activity.changed";
    pub const WORKSPACES: &str = "workspaces.changed";
    pub const MEDIA: &str = "media.changed";
    pub const AUDIO: &str = "audio.changed";
    pub const BRIGHTNESS: &str = "brightness.changed";
    pub const BATTERY: &str = "battery.changed";
    pub const POWER_PROFILE: &str = "power-profile.changed";
    pub const NOTIFICATIONS: &str = "notifications.changed";
    pub const NOTIFICATION_ACTIVE: &str = "notifications.active.changed";
    pub const UPDATES: &str = "updates.changed";
    pub const TIMEZONE: &str = "timezone.changed";
}

pub const METHODS: &[&str] = &[
    "bar.snapshot",
    "activity.queryRange",
    "activity.refresh",
    "todos.create",
    "todos.complete",
    "todos.delete",
    "workspace.focus",
    "media.operation",
    "audio.adjust",
    "audio.setMuted",
    "audio.setInputMuted",
    "brightness.adjust",
    "brightness.set",
    "powerProfile.set",
    "notifications.togglePanel",
    "notifications.toggleDnd",
    "notifications.setDnd",
    "notifications.list",
    "notifications.dismiss",
    "notifications.clear",
    "notifications.invokeAction",
    "notifications.reply",
    "updates.refresh",
];
pub const STREAMS: &[&str] = &[
    stream::ACTIVITY,
    stream::WORKSPACES,
    stream::MEDIA,
    stream::AUDIO,
    stream::BRIGHTNESS,
    stream::BATTERY,
    stream::POWER_PROFILE,
    stream::NOTIFICATIONS,
    stream::NOTIFICATION_ACTIVE,
    stream::UPDATES,
    stream::TIMEZONE,
];

pub fn registry() -> Value {
    json!({
        "protocol": NAME,
        "version": VERSION,
        "methods": [
            { "name": "bar.snapshot", "params": {}, "result": "snapshot" },
            { "name": "activity.queryRange", "params": { "from_unix_ms": 1767225600000_i64, "to_unix_ms": 1769904000000_i64 }, "result": "activity_range" },
            { "name": "activity.refresh", "params": {}, "result": "activity" },
            { "name": "todos.create", "params": { "title": "Plan release", "due_unix_ms": null, "due_date": "2026-01-20", "priority": 3 }, "result": "todo" },
            { "name": "todos.complete", "params": { "id": "local-1", "completed": true }, "result": "todo" },
            { "name": "todos.delete", "params": { "id": "local-1" }, "result": "deleted" },
            { "name": "workspace.focus", "params": { "workspace_id": 1, "on_current_monitor": false }, "result": "operation" },
            { "name": "media.operation", "params": { "operation": "play-pause", "player_id": null }, "result": "operation" },
            { "name": "audio.adjust", "params": { "delta_percent": 5 }, "result": "audio" },
            { "name": "audio.setMuted", "params": { "muted": null }, "result": "audio" },
            { "name": "audio.setInputMuted", "params": { "muted": null }, "result": "audio" },
            { "name": "brightness.adjust", "params": { "delta_percent": 5 }, "result": "brightness" },
            { "name": "brightness.set", "params": { "percent": 50 }, "result": "brightness" },
            { "name": "powerProfile.set", "params": { "profile": "balanced" }, "result": "power_profile" },
            { "name": "notifications.togglePanel", "params": {}, "result": "operation" },
            { "name": "notifications.toggleDnd", "params": {}, "result": "operation" },
            { "name": "notifications.setDnd", "params": { "enabled": true }, "result": "notifications" },
            { "name": "notifications.list", "params": { "before_history_id": null, "limit": 50 }, "result": "notification_history" },
            { "name": "notifications.dismiss", "params": { "id": 1 }, "result": "operation" },
            { "name": "notifications.clear", "params": {}, "result": "operation" },
            { "name": "notifications.invokeAction", "params": { "id": 1, "action_key": "default", "activation_token": null }, "result": "operation" },
            { "name": "notifications.reply", "params": { "id": 1, "text": "Reply" }, "result": "operation" },
            { "name": "updates.refresh", "params": {}, "result": "updates" }
        ],
        "streams": [
            { "name": stream::ACTIVITY, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::WORKSPACES, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::MEDIA, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::AUDIO, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::BRIGHTNESS, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::BATTERY, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::POWER_PROFILE, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::NOTIFICATIONS, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::NOTIFICATION_ACTIVE, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::UPDATES, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::TIMEZONE, "events": ["subscribed", "changed", "lagged"] }
        ]
    })
}

#[cfg(test)]
fn generated_contract_fixture() -> Value {
    json!({
        "protocol": NAME,
        "version": VERSION,
        "registry": registry(),
        "snapshot": {
            "activity": {
                "available": true,
                "syncing": false,
                "event_count": 1,
                "incomplete_todo_count": 1,
                "next_event": {
                    "id": "work:meeting:1768464000000", "source_id": "work", "calendar_name": "Work",
                    "color": "#7aa2f7", "title": "Planning", "start_unix_ms": 1768464000000_i64,
                    "end_unix_ms": 1768467600000_i64, "all_day": false, "start_date": null,
                    "end_date": null, "timezone": "Europe/Amsterdam", "location": "Room 1", "url": ""
                },
                "sources": [{ "id": "work", "name": "Work", "kind": "ics-directory", "available": true, "item_count": 1, "error": null }],
                "world_clocks": [{ "timezone": "Asia/Tokyo", "label": "Tokyo", "city": "Tokyo", "abbreviation": "JST", "utc_offset_seconds": 32400 }],
                "error": null
            },
            "workspaces": {
                "available": true,
                "focused_monitor": "eDP-1",
                "active_window": {
                    "address": "0x123", "title": "Terminal", "class_name": "com.mitchellh.ghostty",
                    "initial_class": "ghostty", "workspace_id": 1, "fullscreen": false, "floating": false
                },
                "monitors": [{ "id": 0, "name": "eDP-1", "focused": true, "active_workspace_id": 1 }],
                "workspaces": [{ "id": 1, "name": "1", "monitor": "eDP-1", "windows": 1, "urgent": false, "fullscreen": false, "last_window_title": "Terminal" }],
                "error": null
            },
            "media": {
                "available": true,
                "active_player": "org.mpris.MediaPlayer2.spotify",
                "players": [{
                    "id": "org.mpris.MediaPlayer2.spotify", "identity": "Spotify", "desktop_entry": "spotify",
                    "playback_status": "playing", "title": "Track", "artist": "Artist", "album": "Album", "art_url": "",
                    "length_us": 240000000, "position_us": 60000000,
                    "position_observed_at_unix_ms": 1234567890000_u64, "playback_rate": 1.0,
                    "can_control": true, "can_play": true, "can_pause": true, "can_next": true, "can_previous": true
                }],
                "error": null
            },
            "audio": {
                "available": true, "sink_name": "alsa_output.test", "sink_description": "Speakers",
                "volume_percent": 50, "muted": false, "input_available": true,
                "source_name": "alsa_input.test", "source_description": "Microphone",
                "input_muted": false, "error": null
            },
            "brightness": {
                "available": true, "device": "amdgpu_bl1", "brightness": 500, "max_brightness": 1000,
                "percent": 50, "error": null
            },
            "battery": {
                "available": true, "native_path": "BAT0", "percentage": 80, "state": "discharging",
                "charging": false, "plugged": false, "power_watts": 8.2, "time_to_empty_seconds": 14400,
                "time_to_full_seconds": 0, "health_percent": 85, "cycles": 101, "warning": false,
                "critical": false, "error": null
            },
            "power_profile": {
                "available": true, "profile": "balanced", "driver": "amd_pstate",
                "profiles": [{ "name": "balanced", "driver": "amd_pstate", "platform_driver": "platform_profile" }],
                "performance_degraded": "", "error": null
            },
            "notifications": {
                "available": true, "count": 1, "dnd": false, "inhibited": false, "text": "1",
                "tooltip": "1 Notification", "alt": "notification", "class_name": "notification",
                "backend": "native", "history_revision": 1, "error": null
            },
            "notification_active": {
                "available": true, "revision": 1,
                "notifications": [{
                    "id": 1, "app_name": "Calendar", "app_icon": "calendar", "summary": "Planning",
                    "body": "Meeting starts soon", "actions": [{ "key": "default", "label": "Open" }],
                    "hints": {
                        "urgency": 1, "category": "calendar", "desktop_entry": "calendar", "image_path": "",
                        "sound_name": "", "sound_file": "", "resident": false, "transient": false,
                        "suppress_sound": false, "image_data_present": false
                    },
                    "created_unix_ms": 1768463900000_u64, "updated_unix_ms": 1768463900000_u64,
                    "expires_unix_ms": 1768463905000_u64, "group_key": "calendar"
                }],
                "error": null
            },
            "updates": {
                "available": true, "ready": true,
                "lanes": [{ "name": "fast", "ready": true, "revision": "abc", "base_hash": "def", "created_at": 123, "auto_apply": false, "system": "/nix/store/system" }],
                "state_directory": "/var/lib/nixos-delayed-updates-v2", "error": null
            },
            "timezone": {
                "available": true, "timezone": "Europe/Amsterdam", "city": "Amsterdam",
                "abbreviation": "CEST", "utc_offset_seconds": 7200, "error": null
            }
        }
    })
}

pub fn contract_fixture() -> Value {
    serde_json::from_str(include_str!("../test_support/bar-api-v1.json"))
        .expect("checked bar-api fixture must be valid JSON")
}

#[cfg(test)]
mod tests {
    use super::{METHODS, STREAMS, contract_fixture, generated_contract_fixture, registry};

    #[test]
    fn checked_contract_fixture_is_current() {
        assert_eq!(contract_fixture(), generated_contract_fixture());
    }

    #[test]
    fn registry_matches_constants() {
        let value = registry();
        let methods = value["methods"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        let streams = value["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(methods, METHODS);
        assert_eq!(streams, STREAMS);
    }
}
