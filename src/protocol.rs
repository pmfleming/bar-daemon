use serde_json::{Value, json};

use crate::api::VERSION;

pub mod stream {
    pub const WORKSPACES: &str = "workspaces.changed";
    pub const MEDIA: &str = "media.changed";
    pub const AUDIO: &str = "audio.changed";
}

pub const METHODS: &[&str] = &[
    "bar.snapshot",
    "workspace.focus",
    "media.operation",
    "audio.adjust",
    "audio.setMuted",
];
pub const STREAMS: &[&str] = &[stream::WORKSPACES, stream::MEDIA, stream::AUDIO];

pub fn registry() -> Value {
    json!({
        "protocol": "bar-api",
        "version": VERSION,
        "methods": [
            { "name": "bar.snapshot", "params": {}, "result": "snapshot" },
            { "name": "workspace.focus", "params": { "workspace_id": 1, "on_current_monitor": false }, "result": "operation" },
            { "name": "media.operation", "params": { "operation": "play-pause", "player_id": null }, "result": "operation" },
            { "name": "audio.adjust", "params": { "delta_percent": 5 }, "result": "audio" },
            { "name": "audio.setMuted", "params": { "muted": null }, "result": "audio" }
        ],
        "streams": [
            { "name": stream::WORKSPACES, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::MEDIA, "events": ["subscribed", "changed", "lagged"] },
            { "name": stream::AUDIO, "events": ["subscribed", "changed", "lagged"] }
        ]
    })
}

pub fn contract_fixture() -> Value {
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
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{METHODS, STREAMS, registry};

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
