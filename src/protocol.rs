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
    pub const WORKAREA: &str = "workarea.changed";
    /// Observed compositor preferences or their availability changed.
    pub const COMPOSITOR: &str = "compositor.changed";
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
    /// Automatic sleep profiles or their runtime status changed.
    pub const SLEEP_POLICY: &str = "sleep-policy.changed";
    /// Laptop display preference or docking recovery status changed.
    pub const DISPLAY_POLICY: &str = "display-policy.changed";
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
    "powerProfile.resumeAutomatic",
    "powerProfile.setBatteryAware",
    "powerProfile.setActionEnabled",
    "powerSleep.lock",
    "powerSleep.suspend",
    "powerSleep.hibernate",
    "powerSleep.setKeepAwake",
    "powerSleep.setPolicy",
    "powerSleep.idle",
    "powerSleep.cancelCritical",
    "powerSleep.setCriticalPolicy",
    "displayPolicy.set",
    "displayFocus.set",
    "displayFocus.reset",
    "displayLayout.preview",
    "displayLayout.confirm",
    "displayLayout.revert",
    "notifications.togglePanel",
    "notifications.toggleDnd",
    "notifications.setDnd",
    "notifications.list",
    "notifications.queryHistory",
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
    stream::WORKAREA,
    stream::COMPOSITOR,
    stream::MEDIA,
    stream::AUDIO,
    stream::BRIGHTNESS,
    stream::BATTERY,
    stream::POWER_PROFILE,
    stream::POWER_SLEEP,
    stream::SLEEP_POLICY,
    stream::DISPLAY_POLICY,
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
            { "name": "media.operation", "params": { "operation": "play-pause", "player_id": null, "offset_seconds": null, "mode": null }, "result": "operation" },
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
            { "name": "battery.setAlertPolicy", "params": { "warning_percent": 25, "critical_percent": 12, "notify_when_full": true, "notify_warning": true, "notify_critical": true, "warning_profile": "power-saver", "critical_profile": "power-saver" }, "result": "battery" },
            { "name": "powerProfile.set", "params": { "profile": "balanced" }, "result": "power_profile" },
            { "name": "powerProfile.resumeAutomatic", "params": {}, "result": "power_profile" },
            { "name": "powerProfile.setBatteryAware", "params": { "enabled": true }, "result": "power_profile" },
            { "name": "powerProfile.setActionEnabled", "params": { "action": "amdgpu_panel_power", "enabled": true }, "result": "power_profile" },
            { "name": "powerSleep.lock", "params": {}, "result": "power_sleep" },
            { "name": "powerSleep.suspend", "params": {}, "result": "power_sleep" },
            { "name": "powerSleep.hibernate", "params": {}, "result": "power_sleep" },
            { "name": "powerSleep.setKeepAwake", "params": { "enabled": true }, "result": "power_sleep" },
            { "name": "powerSleep.setPolicy", "params": { "lid_action": "profile", "same_profile": true, "battery": { "sleep_minutes": 30, "hibernate_minutes": 120 }, "plugged": { "sleep_minutes": 60, "hibernate_minutes": 180 } }, "result": "sleep_policy" },
            { "name": "powerSleep.idle", "params": { "sleep_minutes": 30, "generation": "1234-5678", "episode": 1 }, "result": "power_sleep" },
            { "name": "powerSleep.cancelCritical", "params": {}, "result": "sleep_policy" },
            { "name": "powerSleep.setCriticalPolicy", "params": { "enabled": false, "percent": 5, "grace_seconds": 60 }, "result": "sleep_policy" },
            { "name": "displayPolicy.set", "params": { "prefer_external": true }, "result": "display_policy" },
            { "name": "displayFocus.set", "params": { "values": { "misc:mouse_move_focuses_monitor": false } }, "result": "display_policy" },
            { "name": "displayFocus.reset", "params": {}, "result": "display_policy" },
            { "name": "displayLayout.preview", "params": { "outputs": [{ "name": "eDP-1", "mode": "1920x1200@60", "x": 0, "y": 0, "scale": 1.25, "transform": 0, "enabled": true, "mirror_of": "" }] }, "result": "display_policy" },
            { "name": "displayLayout.confirm", "params": { "id": "preview-id" }, "result": "display_policy" },
            { "name": "displayLayout.revert", "params": { "id": "preview-id" }, "result": "display_policy" },
            { "name": "notifications.togglePanel", "params": {}, "result": "operation" },
            { "name": "notifications.toggleDnd", "params": {}, "result": "operation" },
            { "name": "notifications.setDnd", "params": { "enabled": true, "until_unix_ms": 1768467500000_u64 }, "result": "notifications" },
            { "name": "notifications.list", "params": { "before_history_id": null, "limit": 50 }, "result": "notification_history" },
            { "name": "notifications.queryHistory", "params": { "query": "", "cursor": null, "anchor": null, "limit": 50 }, "result": "notification_page" },
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
            { "name": stream::WORKAREA, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::COMPOSITOR, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::MEDIA, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::AUDIO, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::BRIGHTNESS, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::BATTERY, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::POWER_PROFILE, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::POWER_SLEEP, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::SLEEP_POLICY, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::DISPLAY_POLICY, "events": ["subscribed", "changed", "lagged"] },
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
    // Exercise the real wire projection rather than maintaining a JS-shaped
    // duplicate of the compositor normalization in a hand-written fixture.
    let displays: crate::display_policy::DisplayPolicyState = serde_json::from_value(json!({
        "available": true, "policy": { "prefer_external": true }, "status": "external", "error": null,
        "outputs": [
            { "id": 0, "name": "eDP-1", "width": 1920, "height": 1200, "refreshRate": 60,
              "x": 0, "y": 0, "scale": 1.25, "transform": 0, "disabled": true,
              "availableModes": ["1920x1200@60.00Hz"] },
            { "id": 1, "name": "DP-1", "width": 3840, "height": 2160, "refreshRate": 59.94,
              "x": 1536, "y": 0, "scale": 1.5, "transform": 0, "disabled": false,
              "availableModes": ["3840x2160@59.940Hz", "3840x2160@60.00Hz", "2560x1440@120.00Hz"] }
        ],
        "layout": { "saved": { "outputs": [] }, "trial": null },
        "focus": { "available": true, "values": { "input:follow_mouse": 1, "misc:mouse_move_focuses_monitor": true }, "saved": {}, "error": null }
    })).expect("valid display fixture");
    let weather = json!({
        "available": true, "id": "home", "location": "Amsterdam", "home": true,
        "timezone": "Europe/Amsterdam", "utc_offset_seconds": 3600,
        "timezone_region_ids": ["Europe-Paris"],
        "latitude": 52.3676, "longitude": 4.9041,
        "condition": "Clear", "condition_code": 0, "is_day": true,
        "temperature_c": 17.0, "apparent_temperature_c": 16.0,
        "high_c": 21.0, "low_c": 13.0, "precipitation_probability": 5,
        "precipitation_mm": 0.0, "wind_speed_kmh": 12.0,
        "wind_direction_degrees": 315, "wind_gust_kmh": 18.0, "humidity_percent": 73,
        "sunrise_unix_ms": 1766385120000_i64, "sunset_unix_ms": 1766436420000_i64,
        "updated_unix_ms": 1766400000000_i64,
        "solar_noon": crate::activity::astronomy::solar_noon(1766385120000, 1766436420000, 1766400000000, "Europe/Amsterdam"),
        "hourly": [{ "time_unix_ms": 1766448000000_i64, "temperature_c": 17.0, "precipitation_probability": 5, "precipitation_mm": 0.0, "wind_speed_kmh": 12.0, "wind_direction_degrees": 315, "condition": "Clear", "condition_code": 0, "is_day": true }],
        "daily": [{ "date_unix_ms": 1766361600000_i64, "high_c": 21.0, "low_c": 13.0, "precipitation_probability": 5, "condition": "Clear", "condition_code": 0, "sunrise_unix_ms": 1766385120000_i64, "sunset_unix_ms": 1766436420000_i64 }],
        "error": null
    });
    let mut fixture = json!({
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
                "world_clocks": [{ "timezone": "Asia/Tokyo", "label": "Tokyo", "city": "Tokyo", "abbreviation": "JST", "utc_offset_seconds": 32400, "timezone_region_ids": ["Asia-Tokyo"] }],
                "lunar": crate::activity::astronomy::lunar(1766400000000),
                "weather": weather.clone(),
                "weather_locations": [weather],
                "error": null
            },
            "workarea": { "available": false, "revision": 0, "monitors": {}, "error": null },
            "compositor": crate::model::CompositorState {
                available: true, revision: 1, animations_enabled: Some(false), error: None,
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
                "pinned_player": null,
                "players": [{
                    "id": "org.mpris.MediaPlayer2.spotify", "identity": "Spotify", "desktop_entry": "spotify",
                    "content_type": "music", "control_mode": "automatic",
                    "playback_status": "playing", "title": "Track", "artist": "Artist", "album": "Album", "art_url": "",
                    "length_us": 240000000, "position_us": 60000000,
                    "position_observed_at_unix_ms": 1234567890000_u64, "playback_rate": 1.0,
                    "can_control": true, "can_play": true, "can_pause": true, "can_seek": true, "can_next": true, "can_previous": true
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
                "charging": false, "plugged": false, "power_watts": 8.2, "power_available": true, "time_to_empty_seconds": 14400,
                "forecast": crate::battery::derived::forecast(&crate::model::BatteryState {
                    available: true, percentage: 80, time_to_empty_seconds: 14400,
                    protection: crate::model::BatteryProtectionState { enabled: true, end_percent: Some(80), ..Default::default() },
                    ..Default::default()
                }),
                "time_to_full_seconds": 0, "health_percent": 85, "cycles": 101, "warning": false,
                "critical": false,
                "policy": { "warning_percent": 25, "critical_percent": 12, "notify_when_full": true, "auto_power_saver": true, "notify_warning": true, "notify_critical": true, "warning_profile": "power-saver", "critical_profile": "power-saver", "recovery_margin_percent": 3 },
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
                    "latest_timestamp_ms": 1768464900000_u64, "active_duration_ms": 900000_u64,
                    "energy": crate::battery::derived::energy(&[]),
                    "current_point": crate::model::BatteryHistoryPoint {
                        timestamp_ms: 1768464930000, active_time_ms: 930000,
                        continuous: true, mode: "discharging".into(), percentage: 80,
                        power_watts: 8.2, power_valid: Some(true), ..Default::default()
                    },
                    "points": []
                },
                "error": null
            },
            "power_profile": {
                "available": true, "profile": "balanced", "driver": "amd_pstate",
                "profiles": [{ "name": "balanced", "driver": "amd_pstate", "platform_driver": "platform_profile" }],
                "performance_degraded": "", "version": "0.30", "battery_aware": true,
                "battery_automation": { "level": "normal", "status": "waiting", "profile": "", "error": null },
                "actions": [{ "name": "amdgpu_panel_power", "description": "Panel power savings", "enabled": true }],
                "active_holds": [{ "application_id": "org.example.Compiler", "profile": "performance", "reason": "Building" }],
                "error": null
            },
            "power_sleep": {
                "available": true, "can_suspend": "yes", "can_hibernate": "challenge",
                "preparing_for_sleep": false, "resume_generation": 0, "lock_before_sleep": true,
                "operation": { "id": 0, "action": "", "phase": "", "job": null, "error": null },
                "keep_awake": false,
                "diagnostics": crate::sleep::diagnostics::SleepDiagnostics::default(),
                "inhibitors": [{ "what": "sleep", "who": "Backup", "why": "Writing snapshot", "mode": "delay", "uid": 1000, "pid": 4242 }],
                "error": null
            },
            "display_policy": displays,
            "sleep_policy": {
                "available": true, "active_profile": "shared",
                "hibernate_available": true, "hibernate_ready": true, "hibernate_error": null, "last_error": null,
                "lid": { "available": true, "managed": true, "error": null },
                "policy": { "lid_action": "profile", "same_profile": true, "critical_battery": { "enabled": false, "percent": 5, "grace_seconds": 60 }, "battery": { "sleep_minutes": 30, "hibernate_minutes": 120 }, "plugged": { "sleep_minutes": 60, "hibernate_minutes": 180 } },
                "critical_battery": { "phase": "disabled", "remaining_seconds": 0, "error": null },
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
                    "expires_unix_ms": null, "toast_visible": true,
                    "toast_expires_unix_ms": 1768463905000_u64, "group_key": "calendar",
                    "source_monitor": "eDP-1", "snoozed_until_unix_ms": null
                }],
                "error": null
            },
            "updates": {
                "available": true, "ready": true,
                "lanes": [{ "name": "delayed", "ready": true, "revision": "abc", "base_hash": "def", "created_at": 123, "auto_apply": false, "system": "/nix/store/system" }],
                "jobs": [{ "name": "system", "operation": "run-delayed", "status": "completed", "phase": "ready", "started_at": 123, "finished_at": 124, "exit_code": 0, "error": null }],
                "state_directory": "/var/lib/nixos-delayed-updates-v2", "error": null
            },
            "timezone": {
                "available": true, "timezone": "Europe/Amsterdam", "city": "Amsterdam",
                "abbreviation": "CEST", "utc_offset_seconds": 7200,
                "timezone_region_ids": ["Africa-Johannesburg", "Europe-Paris"], "error": null
            }
        }
    });
    let active =
        serde_json::from_value(fixture["snapshot"]["notification_active"]["notifications"].clone())
            .expect("notification fixture");
    let page = crate::activity::notifications::history::page(
        Vec::new(),
        active,
        &crate::activity::notifications::history::HistoryQuery {
            query: String::new(),
            cursor: None,
            anchor: None,
            limit: 50,
        },
        None,
        "contract-epoch",
        1,
        1768463900000,
    )
    .expect("native catalog projection");
    fixture["notification_page"] = serde_json::to_value(page).expect("catalog wire projection");
    fixture
}

/// Loads the checked protocol contract fixture shipped with the crate.
pub fn contract_fixture() -> serde_json::Result<Value> {
    shelllist_daemon_core::load_fixture(include_str!("../test_support/bar-api-v1.json"))
}

#[cfg(test)]
mod tests {
    use super::{contract_fixture, generated_contract_fixture};

    #[test]
    fn checked_contract_fixture_is_current() -> serde_json::Result<()> {
        let actual = generated_contract_fixture();
        if std::env::var_os("BAR_DAEMON_UPDATE_CONTRACT_FIXTURE").is_some() {
            std::fs::write(
                "test_support/bar-api-v1.json",
                format!("{}\n", serde_json::to_string_pretty(&actual)?),
            )
            .expect("write contract fixture");
            return Ok(());
        }
        assert_eq!(contract_fixture()?, actual);
        Ok(())
    }
}
