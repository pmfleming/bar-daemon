use serde_json::{Value, json};

use crate::api::VERSION;

pub mod stream {
    pub const WORKSPACES: &str = "workspaces.changed";
    pub const MEDIA: &str = "media.changed";
    pub const AUDIO: &str = "audio.changed";
    pub const BRIGHTNESS: &str = "brightness.changed";
    pub const BATTERY: &str = "battery.changed";
    pub const POWER_PROFILE: &str = "power-profile.changed";
    pub const NOTIFICATIONS: &str = "notifications.changed";
    pub const UPDATES: &str = "updates.changed";
    pub const TIMEZONE: &str = "timezone.changed";
}

pub const METHODS: &[&str] = &[
    "bar.snapshot",
    "workspace.focus",
    "media.operation",
    "audio.adjust",
    "audio.setMuted",
    "brightness.adjust",
    "brightness.set",
    "powerProfile.set",
    "notifications.togglePanel",
    "notifications.toggleDnd",
    "updates.refresh",
];
pub const STREAMS: &[&str] = &[
    stream::WORKSPACES,
    stream::MEDIA,
    stream::AUDIO,
    stream::BRIGHTNESS,
    stream::BATTERY,
    stream::POWER_PROFILE,
    stream::NOTIFICATIONS,
    stream::UPDATES,
    stream::TIMEZONE,
];

pub fn registry() -> Value {
    json!({
        "protocol": "bar-api",
        "version": VERSION,
        "methods": [
            { "name": "bar.snapshot", "params": {}, "result": "snapshot" },
            { "name": "workspace.focus", "params": { "workspace_id": 1, "on_current_monitor": false }, "result": "operation" },
            { "name": "media.operation", "params": { "operation": "play-pause", "player_id": null }, "result": "operation" },
            { "name": "audio.adjust", "params": { "delta_percent": 5 }, "result": "audio" },
            { "name": "audio.setMuted", "params": { "muted": null }, "result": "audio" },
            { "name": "brightness.adjust", "params": { "delta_percent": 5 }, "result": "brightness" },
            { "name": "brightness.set", "params": { "percent": 50 }, "result": "brightness" },
            { "name": "powerProfile.set", "params": { "profile": "balanced" }, "result": "power_profile" },
            { "name": "notifications.togglePanel", "params": {}, "result": "operation" },
            { "name": "notifications.toggleDnd", "params": {}, "result": "operation" },
            { "name": "updates.refresh", "params": {}, "result": "updates" }
        ],
        "streams": [
            { "name": stream::WORKSPACES, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::MEDIA, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::AUDIO, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::BRIGHTNESS, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::BATTERY, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::POWER_PROFILE, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::NOTIFICATIONS, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::UPDATES, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::TIMEZONE, "events": ["subscribed", "changed", "lagged"] }
        ]
    })
}

#[cfg(test)]
fn generated_contract_fixture() -> Value {
    json!({
        "protocol": "bar-api",
        "version": VERSION,
        "registry": registry(),
        "snapshot": {
            "workspaces": {
                "available": true,
                "focused_monitor": "eDP-1",
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
                    "can_control": true, "can_play": true, "can_pause": true, "can_next": true, "can_previous": true
                }],
                "error": null
            },
            "audio": {
                "available": true, "sink_name": "alsa_output.test", "sink_description": "Speakers",
                "volume_percent": 50, "muted": false, "error": null
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
                "tooltip": "1 Notification", "alt": "notification", "class_name": "notification", "error": null
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
