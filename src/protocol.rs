use serde_json::{Value, json};

/// Stable protocol family name.
pub const NAME: &str = "bar-api";
/// Current protocol contract version.
pub const VERSION: u8 = 1;

/// Event stream names accepted by subscriptions.
pub mod stream {
    /// Activity summary changed.
    pub const ACTIVITY: &str = "activity.changed";
    /// Workspace state changed.
    pub const WORKSPACES: &str = "workspaces.changed";
    /// Media player state changed.
    pub const MEDIA: &str = "media.changed";
    /// Audio state changed.
    pub const AUDIO: &str = "audio.changed";
    /// Backlight state changed.
    pub const BRIGHTNESS: &str = "brightness.changed";
    /// Battery state changed.
    pub const BATTERY: &str = "battery.changed";
    /// Power profile state changed.
    pub const POWER_PROFILE: &str = "power-profile.changed";
    /// Sleep capability or inhibitor state changed.
    pub const POWER_SLEEP: &str = "power-sleep.changed";
    /// Keyboard LED, keyboard backlight, or privacy hardware state changed.
    pub const OSD_HARDWARE: &str = "osd-hardware.changed";
    /// Notification summary changed.
    pub const NOTIFICATIONS: &str = "notifications.changed";
    /// Active notification collection changed.
    pub const NOTIFICATION_ACTIVE: &str = "notifications.active.changed";
    /// Update readiness changed.
    pub const UPDATES: &str = "updates.changed";
    /// System timezone changed.
    pub const TIMEZONE: &str = "timezone.changed";
}

/// Complete ordered method registry for protocol version 1.
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
    "battery.history",
    "battery.setThresholds",
    "battery.setProtection",
    "battery.chargeOnce",
    "battery.setChargingInhibited",
    "battery.startCalibration",
    "battery.cancelCalibration",
    "battery.setAlertPolicy",
    "powerProfile.set",
    "powerProfile.setBatteryAware",
    "powerProfile.setActionEnabled",
    "powerSleep.lock",
    "powerSleep.suspend",
    "powerSleep.hibernate",
    "notifications.togglePanel",
    "notifications.toggleDnd",
    "notifications.setDnd",
    "notifications.list",
    "notifications.dismiss",
    "notifications.clear",
    "notifications.clearGroup",
    "notifications.snooze",
    "notifications.invokeAction",
    "notifications.reply",
    "updates.refresh",
];
/// Complete ordered event-stream registry for protocol version 1.
pub const STREAMS: &[&str] = &[
    stream::ACTIVITY,
    stream::WORKSPACES,
    stream::MEDIA,
    stream::AUDIO,
    stream::BRIGHTNESS,
    stream::BATTERY,
    stream::POWER_PROFILE,
    stream::POWER_SLEEP,
    stream::OSD_HARDWARE,
    stream::NOTIFICATIONS,
    stream::NOTIFICATION_ACTIVE,
    stream::UPDATES,
    stream::TIMEZONE,
];

/// Returns machine-readable method and stream metadata.
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
            { "name": "battery.history", "params": {}, "result": "history" },
            { "name": "battery.setThresholds", "params": { "battery_id": "BAT0", "start_percent": 75, "end_percent": 80 }, "result": "battery" },
            { "name": "battery.setProtection", "params": { "battery_id": "BAT0", "enabled": true, "start_percent": 75, "end_percent": 80 }, "result": "battery" },
            { "name": "battery.chargeOnce", "params": { "battery_id": "BAT0" }, "result": "battery" },
            { "name": "battery.setChargingInhibited", "params": { "battery_id": "BAT0", "enabled": true }, "result": "battery" },
            { "name": "battery.startCalibration", "params": { "battery_id": "BAT0" }, "result": "battery" },
            { "name": "battery.cancelCalibration", "params": { "battery_id": "BAT0" }, "result": "battery" },
            { "name": "battery.setAlertPolicy", "params": { "warning_percent": 25, "critical_percent": 12, "notify_when_full": true, "auto_power_saver": true }, "result": "battery" },
            { "name": "powerProfile.set", "params": { "profile": "balanced" }, "result": "power_profile" },
            { "name": "powerProfile.setBatteryAware", "params": { "enabled": true }, "result": "power_profile" },
            { "name": "powerProfile.setActionEnabled", "params": { "action": "amdgpu_panel_power", "enabled": true }, "result": "power_profile" },
            { "name": "powerSleep.lock", "params": {}, "result": "power_sleep" },
            { "name": "powerSleep.suspend", "params": {}, "result": "power_sleep" },
            { "name": "powerSleep.hibernate", "params": {}, "result": "power_sleep" },
            { "name": "notifications.togglePanel", "params": {}, "result": "operation" },
            { "name": "notifications.toggleDnd", "params": {}, "result": "operation" },
            { "name": "notifications.setDnd", "params": { "enabled": true, "until_unix_ms": 1768467500000_u64 }, "result": "notifications" },
            { "name": "notifications.list", "params": { "before_history_id": null, "limit": 50 }, "result": "notification_history" },
            { "name": "notifications.dismiss", "params": { "id": 1 }, "result": "operation" },
            { "name": "notifications.clear", "params": {}, "result": "operation" },
            { "name": "notifications.clearGroup", "params": { "group_key": "calendar" }, "result": "operation" },
            { "name": "notifications.snooze", "params": { "id": 1, "until_unix_ms": 1768467500000_u64 }, "result": "operation" },
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
            { "name": stream::POWER_SLEEP, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::OSD_HARDWARE, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::NOTIFICATIONS, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::NOTIFICATION_ACTIVE, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::UPDATES, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::TIMEZONE, "events": ["subscribed", "changed", "lagged"] }
        ]
    })
}

#[cfg(test)]
fn generated_contract_fixture() -> Value {
    let weather = json!({
        "available": true, "id": "home", "location": "Amsterdam", "home": true,
        "timezone": "Europe/Amsterdam", "utc_offset_seconds": 7200,
        "condition": "Clear", "temperature_c": 17.0, "apparent_temperature_c": 16.0,
        "high_c": 21.0, "low_c": 13.0, "precipitation_probability": 5,
        "wind_speed_kmh": 12.0, "humidity_percent": 73,
        "sunrise_unix_ms": 1766385120000_i64, "sunset_unix_ms": 1766436420000_i64,
        "updated_unix_ms": 1766446800000_i64,
        "hourly": [{ "time_unix_ms": 1766448000000_i64, "temperature_c": 17.0, "precipitation_probability": 5, "wind_speed_kmh": 12.0, "condition": "Clear" }],
        "daily": [{ "date_unix_ms": 1766361600000_i64, "high_c": 21.0, "low_c": 13.0, "precipitation_probability": 5, "condition": "Clear", "sunrise_unix_ms": 1766385120000_i64, "sunset_unix_ms": 1766436420000_i64 }],
        "error": null
    });
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
                "weather": weather.clone(),
                "weather_locations": [weather],
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
                "critical": false,
                "policy": { "warning_percent": 25, "critical_percent": 12, "notify_when_full": true, "auto_power_saver": true },
                "operation": {
                    "kind": "", "battery_id": "", "phase": "",
                    "started_unix_ms": 0, "expires_unix_ms": 0
                },
                "protection": {
                    "supported": true, "backend": "thinkpad-sysfs", "managed": true, "enabled": true,
                    "start_percent": 75, "end_percent": 80,
                    "desired_start_percent": 75, "desired_end_percent": 80,
                    "desired_enabled": true, "thresholds_verified": true,
                    "charge_once_active": false, "supports_start": true,
                    "supports_end": true, "supports_charge_behaviour": true,
                    "charge_behaviour": "auto",
                    "available_behaviours": ["auto", "inhibit-charge", "force-discharge"], "error": null
                },
                "devices": [{
                    "id": "BAT0", "vendor": "Sunwoda", "model": "5B11H56418", "serial": "5878",
                    "present": true, "percentage": 80, "state": "discharging", "power_watts": 8.2,
                    "energy_now_wh": 35.8, "energy_full_wh": 44.75, "energy_full_design_wh": 52.5,
                    "voltage_volts": 15.48, "health_percent": 85, "cycles": 101,
                    "protection": {
                        "supported": true, "backend": "thinkpad-sysfs", "managed": true, "enabled": true,
                        "start_percent": 75, "end_percent": 80,
                        "desired_start_percent": 75, "desired_end_percent": 80,
                        "desired_enabled": true, "thresholds_verified": true,
                        "charge_once_active": false, "supports_start": true,
                        "supports_end": true, "supports_charge_behaviour": true,
                        "charge_behaviour": "auto",
                        "available_behaviours": ["auto", "inhibit-charge", "force-discharge"], "error": null
                    }
                }],
                "history": {
                    "retention_days": 7, "last_charge_timestamp_ms": 1768464000000_u64,
                    "latest_timestamp_ms": 1768464900000_u64, "points": []
                },
                "error": null
            },
            "power_profile": {
                "available": true, "profile": "balanced", "driver": "amd_pstate",
                "profiles": [{ "name": "balanced", "driver": "amd_pstate", "platform_driver": "platform_profile" }],
                "performance_degraded": "", "version": "0.30", "battery_aware": true,
                "actions": [{ "name": "amdgpu_panel_power", "description": "Panel power savings", "enabled": true }],
                "active_holds": [{ "application_id": "org.example.Compiler", "profile": "performance", "reason": "Building" }],
                "error": null
            },
            "power_sleep": {
                "available": true, "can_suspend": "yes", "can_hibernate": "challenge",
                "preparing_for_sleep": false, "lock_before_sleep": true,
                "inhibitors": [{ "what": "sleep", "who": "Backup", "why": "Writing snapshot", "mode": "delay", "uid": 1000, "pid": 4242 }],
                "error": null
            },
            "osd_hardware": {
                "available": true, "caps_lock": false, "num_lock": true,
                "keyboard_backlight_percent": 50, "microphone_privacy": false,
                "camera_privacy": true, "error": null
            },
            "notifications": {
                "available": true, "count": 1, "dnd": true, "inhibited": false, "text": "1",
                "tooltip": "1 Notification", "alt": "dnd-notification", "class_name": "dnd-notification",
                "backend": "native", "history_revision": 1,
                "dnd_until_unix_ms": 1768467500000_u64, "error": null
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
                    "expires_unix_ms": 1768463905000_u64, "group_key": "calendar",
                    "source_monitor": "eDP-1", "snoozed_until_unix_ms": null
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

/// Loads the checked protocol contract fixture shipped with the crate.
pub fn contract_fixture() -> serde_json::Result<Value> {
    shelllist_daemon_core::load_fixture(include_str!("../test_support/bar-api-v1.json"))
}

#[cfg(test)]
mod tests {
    use super::{METHODS, STREAMS, contract_fixture, generated_contract_fixture, registry};

    #[test]
    fn checked_contract_fixture_is_current() -> serde_json::Result<()> {
        assert_eq!(contract_fixture()?, generated_contract_fixture());
        Ok(())
    }

    #[test]
    fn registry_matches_constants() {
        let value = registry();
        let methods = shelllist_daemon_core::fixture_names(
            &serde_json::json!({ "registry": { "methods": value["methods"] } }),
            "methods",
        )
        .unwrap()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let streams = shelllist_daemon_core::fixture_names(
            &serde_json::json!({ "registry": { "streams": value["streams"] } }),
            "streams",
        )
        .unwrap()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        assert_eq!(methods, METHODS);
        assert_eq!(streams, STREAMS);
        shelllist_daemon_core::validate_unique_names(METHODS).unwrap();
        shelllist_daemon_core::validate_unique_names(STREAMS).unwrap();
    }
}
