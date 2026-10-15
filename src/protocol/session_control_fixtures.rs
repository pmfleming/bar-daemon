//! Offline session-control contracts. Preference examples use real in-memory selection.
use super::{
    read_wire,
    session_controls::{self, ErrorCode, MediaRequest, WorkspaceRequest},
};
use crate::{
    api,
    media::MediaService,
    model::{MediaControlMode, MediaPlayer, MediaState, UpdateState},
    state::StateStore,
};
use serde_json::{Map, Value, json};

pub(super) async fn fixture() -> Value {
    let players = ["one", "two"].map(|name| MediaPlayer {
        id: format!("org.mpris.MediaPlayer2.contract_{name}"),
        identity: name.into(),
        title: "Example track".into(),
        playback_status: "Playing".into(),
        playback_rate: 1.0,
        can_control: true,
        can_play: true,
        can_pause: true,
        can_seek: true,
        can_next: true,
        can_previous: true,
        length_us: 180_000_000,
        ..Default::default()
    });
    let base = MediaState {
        available: true,
        active_player: Some(players[0].id.clone()),
        players: players.to_vec(),
        ..Default::default()
    };
    let store = StateStore::default();
    store.update_media(base.clone()).await;
    let snapshot = read_wire::snapshot(&store.snapshot().await);
    let mut cases = Map::new();
    for operation in [
        "play-pause",
        "play",
        "pause",
        "stop",
        "next",
        "previous",
        "seek",
        "cycle",
        "automatic",
        "select",
        "set-mode",
    ] {
        let preference = ["cycle", "automatic", "select", "set-mode"].contains(&operation);
        let player_id = if ["cycle", "automatic"].contains(&operation) {
            None
        } else {
            Some(players[0].id.clone())
        };
        let params = MediaRequest {
            operation: operation.into(),
            player_id,
            offset_seconds: (operation == "seek").then_some(10),
            mode: (operation == "set-mode").then_some(MediaControlMode::Seek),
        };
        let success = if preference {
            let store = StateStore::default();
            store.update_media(base.clone()).await;
            let service = MediaService::default();
            let result = match operation {
                "cycle" => service.cycle(&store).await,
                "automatic" => service.select(&store, None).await,
                "select" => service.select(&store, params.player_id.as_deref()).await,
                _ => {
                    service
                        .set_mode(
                            &store,
                            params.player_id.as_deref().unwrap(),
                            MediaControlMode::Seek,
                        )
                        .await
                }
            };
            session_controls::preference(operation, result)
        } else if operation == "seek" {
            session_controls::seek(operation, 10, Ok(players[0].id.clone()))
        } else {
            session_controls::playback(operation, Ok(players[0].id.clone()))
        };
        cases.insert(operation.into(), json!({"method":"media.operation", "params":params, "success":success,
            "failed":ErrorCode::Media.reply("Player disappeared; command outcome unknown"),
            "rejected":api::decode_request::<MediaRequest>(Value::Null, "media.operation").err().unwrap()}));
    }
    let request = WorkspaceRequest {
        workspace_id: 2,
        on_current_monitor: true,
    };
    cases.insert("workspace".into(), json!({"method":"workspace.focus", "params":request,
        "success":session_controls::workspace(&request, Ok(())),
        "failed":ErrorCode::Workspace.reply("Compositor reply lost; focus outcome unknown"),
        "rejected":api::decode_request::<WorkspaceRequest>(Value::Null, "workspace.focus").err().unwrap()}));
    cases.insert("updates".into(), json!({"method":"updates.refresh", "params":{},
        "success":session_controls::updates(Ok(UpdateState { available:true, ..Default::default() })),
        "failed":ErrorCode::Updates.reply("Update state unavailable"), "rejected":null}));
    json!({"snapshot":snapshot, "cases":cases})
}
