use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tokio::{
    sync::{RwLock, mpsc},
    task::JoinSet,
    time::sleep,
};
use zvariant::OwnedValue;

use crate::{
    model::{MediaPlayer, MediaState},
    state::StateStore,
    time::unix_ms as unix_time_ms,
};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Clone, Default)]
pub(crate) struct MediaService {
    selected_player: Arc<RwLock<Option<String>>>,
}

impl MediaService {
    pub(crate) async fn cycle(&self, store: &StateStore) -> Result<MediaState> {
        let mut selected = self.selected_player.write().await;
        let mut state = store.snapshot().await.media;
        let next = next_player_id(&state.players, state.active_player.as_deref())
            .context("no alternate MPRIS player is available")?;
        *selected = Some(next.clone());
        state.active_player = Some(next);
        store.update_media(state.clone()).await;
        Ok(state)
    }

    async fn publish(&self, store: &StateStore, players: Vec<MediaPlayer>) {
        let mut selected = self.selected_player.write().await;
        let active_player = match selected.as_ref() {
            Some(id) if players.iter().any(|player| &player.id == id) => Some(id.clone()),
            _ => {
                *selected = None;
                select_active_player(&players).map(|player| player.id.clone())
            }
        };
        store
            .update_media(MediaState {
                available: !players.is_empty(),
                active_player,
                players,
                error: None,
            })
            .await;
    }
}

pub(crate) async fn monitor(store: StateStore, service: MediaService) {
    loop {
        match zbus::Connection::session().await {
            Ok(connection) => {
                if let Err(error) = monitor_connection(&connection, &store, &service).await {
                    tracing::warn!(%error, "MPRIS monitor disconnected");
                }
            }
            Err(error) => {
                store
                    .update_media(MediaState {
                        error: Some(error.to_string()),
                        ..MediaState::default()
                    })
                    .await;
            }
        }
        sleep(Duration::from_secs(2)).await;
    }
}

async fn monitor_connection(
    connection: &zbus::Connection,
    store: &StateStore,
    service: &MediaService,
) -> Result<()> {
    let dbus = zbus::fdo::DBusProxy::new(connection).await?;
    let mut owner_changes = dbus.receive_name_owner_changed().await?;
    let (changes_tx, mut changes_rx) = mpsc::channel::<()>(32);
    let mut watchers = JoinSet::new();
    let mut names = Vec::new();

    refresh(
        connection,
        store,
        service,
        &mut names,
        &mut watchers,
        &changes_tx,
    )
    .await;
    loop {
        tokio::select! {
            signal = owner_changes.next() => {
                let Some(signal) = signal else { bail!("D-Bus owner-change stream ended"); };
                let args = signal.args()?;
                if args.name().as_str().starts_with(PREFIX) {
                    refresh(connection, store, service, &mut names, &mut watchers, &changes_tx).await;
                }
            }
            changed = changes_rx.recv() => {
                if changed.is_none() { bail!("MPRIS property watcher ended"); }
                refresh_players(connection, store, service, &names).await;
            }
        }
    }
}

async fn refresh(
    connection: &zbus::Connection,
    store: &StateStore,
    service: &MediaService,
    watched_names: &mut Vec<String>,
    watchers: &mut JoinSet<()>,
    changes_tx: &mpsc::Sender<()>,
) {
    match player_names(connection).await {
        Ok(next_names) => {
            if *watched_names != next_names {
                watchers.abort_all();
                for name in &next_names {
                    let connection = connection.clone();
                    let name = name.clone();
                    let tx = changes_tx.clone();
                    watchers.spawn(async move {
                        watch_properties(connection, name, tx).await;
                    });
                }
                *watched_names = next_names;
            }
            refresh_players(connection, store, service, watched_names).await;
        }
        Err(error) => {
            store
                .update_media(MediaState {
                    error: Some(error.to_string()),
                    ..MediaState::default()
                })
                .await
        }
    }
}

async fn refresh_players(
    connection: &zbus::Connection,
    store: &StateStore,
    service: &MediaService,
    names: &[String],
) {
    let mut players = Vec::new();
    for name in names {
        match read_player(connection, name).await {
            Ok(player) => players.push(player),
            Err(error) => {
                tracing::debug!(player = %name, %error, "MPRIS player unavailable during refresh")
            }
        }
    }
    players.sort_by(|a, b| {
        a.identity
            .to_lowercase()
            .cmp(&b.identity.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    service.publish(store, players).await;
}

async fn player_names(connection: &zbus::Connection) -> Result<Vec<String>> {
    let proxy = zbus::fdo::DBusProxy::new(connection).await?;
    let mut names = proxy
        .list_names()
        .await?
        .into_iter()
        .map(|name| name.to_string())
        .filter(|name| name.starts_with(PREFIX))
        .collect::<Vec<_>>();
    names.sort();
    Ok(names)
}

async fn watch_properties(connection: zbus::Connection, name: String, changed: mpsc::Sender<()>) {
    let Ok(properties_proxy) = zbus::Proxy::new(
        &connection,
        name.as_str(),
        PATH,
        "org.freedesktop.DBus.Properties",
    )
    .await
    else {
        return;
    };
    let Ok(player_proxy) =
        zbus::Proxy::new(&connection, name.as_str(), PATH, PLAYER_INTERFACE).await
    else {
        return;
    };
    let Ok(mut properties) = properties_proxy.receive_signal("PropertiesChanged").await else {
        return;
    };
    let Ok(mut seeks) = player_proxy.receive_signal("Seeked").await else {
        return;
    };
    loop {
        let received = tokio::select! {
            signal = properties.next() => signal.is_some(),
            signal = seeks.next() => signal.is_some(),
        };
        if !received || changed.send(()).await.is_err() {
            return;
        }
    }
}

async fn read_player(connection: &zbus::Connection, name: &str) -> Result<MediaPlayer> {
    let root = zbus::Proxy::new(connection, name, PATH, ROOT_INTERFACE).await?;
    let player = zbus::Proxy::new(connection, name, PATH, PLAYER_INTERFACE).await?;
    let identity = root
        .get_property::<String>("Identity")
        .await
        .unwrap_or_else(|_| name.trim_start_matches(PREFIX).to_string());
    let desktop_entry = root
        .get_property::<String>("DesktopEntry")
        .await
        .unwrap_or_default();
    let playback_status = player
        .get_property::<String>("PlaybackStatus")
        .await
        .unwrap_or_else(|_| "Stopped".into())
        .to_lowercase();
    let metadata = player
        .get_property::<HashMap<String, OwnedValue>>("Metadata")
        .await
        .unwrap_or_default();
    let length_us = property_i64(&metadata, "mpris:length")
        .unwrap_or_default()
        .max(0) as u64;
    let position_us = player
        .get_property::<i64>("Position")
        .await
        .unwrap_or_default()
        .max(0) as u64;
    let playback_rate = player.get_property::<f64>("Rate").await.unwrap_or(1.0);
    Ok(MediaPlayer {
        id: name.to_string(),
        identity,
        desktop_entry,
        playback_status,
        title: property_string(&metadata, "xesam:title").unwrap_or_default(),
        artist: property_strings(&metadata, "xesam:artist").join(", "),
        album: property_string(&metadata, "xesam:album").unwrap_or_default(),
        art_url: property_string(&metadata, "mpris:artUrl").unwrap_or_default(),
        length_us,
        position_us,
        position_observed_at_unix_ms: unix_time_ms(),
        playback_rate,
        can_control: player.get_property("CanControl").await.unwrap_or(false),
        can_play: player.get_property("CanPlay").await.unwrap_or(false),
        can_pause: player.get_property("CanPause").await.unwrap_or(false),
        can_next: player.get_property("CanGoNext").await.unwrap_or(false),
        can_previous: player.get_property("CanGoPrevious").await.unwrap_or(false),
    })
}

fn property_string(values: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    values
        .get(key)
        .and_then(|value| <&str>::try_from(value).ok())
        .map(str::to_string)
}

fn property_i64(values: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    values
        .get(key)
        .and_then(|value| i64::try_from(value).ok())
        .or_else(|| {
            values
                .get(key)
                .and_then(|value| u64::try_from(value).ok())
                .and_then(|value| i64::try_from(value).ok())
        })
}

fn property_strings(values: &HashMap<String, OwnedValue>, key: &str) -> Vec<String> {
    values
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<String>::try_from(value).ok())
        .unwrap_or_default()
}

fn next_player_id(players: &[MediaPlayer], current: Option<&str>) -> Option<String> {
    if players.len() < 2 {
        return None;
    }
    let next = players
        .iter()
        .position(|player| Some(player.id.as_str()) == current)
        .map_or(0, |index| (index + 1) % players.len());
    Some(players[next].id.clone())
}

pub(crate) fn select_active_player(players: &[MediaPlayer]) -> Option<&MediaPlayer> {
    players
        .iter()
        .find(|player| player.playback_status == "playing")
        .or_else(|| {
            players.iter().find(|player| {
                player.desktop_entry.eq_ignore_ascii_case("spotify")
                    || player.identity.eq_ignore_ascii_case("spotify")
            })
        })
        .or_else(|| players.iter().find(|player| player.can_control))
        .or_else(|| players.first())
}

fn operation_method(operation: &str) -> Result<&'static str> {
    match operation {
        "play-pause" => Ok("PlayPause"),
        "play" => Ok("Play"),
        "pause" => Ok("Pause"),
        "stop" => Ok("Stop"),
        "next" => Ok("Next"),
        "previous" => Ok("Previous"),
        _ => bail!("unsupported media operation: {operation}"),
    }
}

async fn resolve_player(connection: &zbus::Connection, player_id: Option<&str>) -> Result<String> {
    let names = player_names(connection).await?;
    if let Some(id) = player_id {
        if !names.iter().any(|name| name == id) {
            bail!("requested MPRIS player is unavailable");
        }
        return Ok(id.to_string());
    }
    let mut players = Vec::new();
    for name in &names {
        if let Ok(player) = read_player(connection, name).await {
            players.push(player);
        }
    }
    select_active_player(&players)
        .map(|player| player.id.clone())
        .context("no MPRIS player is available")
}

fn seek_offset_microseconds(offset_seconds: i64) -> Result<i64> {
    if offset_seconds == 0 || offset_seconds.unsigned_abs() > 86_400 {
        bail!("MPRIS seek offset must be between -86400 and 86400 seconds and nonzero");
    }
    offset_seconds
        .checked_mul(1_000_000)
        .context("MPRIS seek offset overflow")
}

pub(crate) async fn seek(player_id: Option<&str>, offset_seconds: i64) -> Result<String> {
    let offset_microseconds = seek_offset_microseconds(offset_seconds)?;
    let connection = zbus::Connection::session()
        .await
        .context("connect to session D-Bus")?;
    let selected = resolve_player(&connection, player_id).await?;
    let proxy = zbus::Proxy::new(&connection, selected.as_str(), PATH, PLAYER_INTERFACE).await?;
    proxy
        .call_method("Seek", &(offset_microseconds,))
        .await
        .context("call MPRIS player seek")?;
    Ok(selected)
}

pub(crate) async fn operation(player_id: Option<&str>, operation: &str) -> Result<String> {
    let method = operation_method(operation)?;
    let connection = zbus::Connection::session()
        .await
        .context("connect to session D-Bus")?;
    let selected = resolve_player(&connection, player_id).await?;
    let proxy = zbus::Proxy::new(&connection, selected.as_str(), PATH, PLAYER_INTERFACE).await?;
    proxy
        .call_method(method, &())
        .await
        .context("call MPRIS player operation")?;
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::{
        MediaService, next_player_id, operation_method, seek_offset_microseconds,
        select_active_player,
    };
    use crate::{model::MediaPlayer, state::StateStore};

    fn player(id: &str, status: &str, spotify: bool, controllable: bool) -> MediaPlayer {
        MediaPlayer {
            id: id.into(),
            identity: if spotify { "Spotify".into() } else { id.into() },
            desktop_entry: String::new(),
            playback_status: status.into(),
            can_control: controllable,
            ..MediaPlayer::default()
        }
    }

    #[test]
    fn cycles_players_and_requires_an_alternative() {
        let players = vec![
            player("browser", "paused", false, true),
            player("spotify", "paused", true, true),
        ];
        assert_eq!(
            next_player_id(&players, Some("browser")).as_deref(),
            Some("spotify")
        );
        assert_eq!(
            next_player_id(&players, Some("spotify")).as_deref(),
            Some("browser")
        );
        assert_eq!(
            next_player_id(&players, Some("missing")).as_deref(),
            Some("browser")
        );
        assert!(next_player_id(&players[..1], Some("browser")).is_none());
    }

    #[tokio::test]
    async fn manual_cycle_is_retained_by_monitor_selection() {
        let store = StateStore::default();
        let service = MediaService::default();
        let players = vec![
            player("browser", "paused", false, true),
            player("spotify", "paused", true, true),
        ];
        store
            .update_media(crate::model::MediaState {
                available: true,
                active_player: Some("spotify".into()),
                players: players.clone(),
                error: None,
            })
            .await;

        let state = service.cycle(&store).await.unwrap();

        assert_eq!(state.active_player.as_deref(), Some("browser"));
        service.publish(&store, players).await;
        assert_eq!(
            store.snapshot().await.media.active_player.as_deref(),
            Some("browser")
        );
    }

    #[test]
    fn validates_and_converts_seek_offsets() {
        assert_eq!(seek_offset_microseconds(-15).unwrap(), -15_000_000);
        assert_eq!(seek_offset_microseconds(30).unwrap(), 30_000_000);
        assert!(seek_offset_microseconds(0).is_err());
        assert!(seek_offset_microseconds(86_401).is_err());
    }

    #[test]
    fn maps_operations_before_accessing_dbus() {
        assert_eq!(operation_method("play-pause").unwrap(), "PlayPause");
        assert_eq!(operation_method("next").unwrap(), "Next");
        assert_eq!(operation_method("previous").unwrap(), "Previous");
        assert!(operation_method("shuffle").is_err());
    }

    #[test]
    fn selects_playing_then_spotify_then_controllable_player() {
        let players = vec![
            player("browser", "paused", false, true),
            player("spotify", "paused", true, true),
            player("playing", "playing", false, true),
        ];
        assert_eq!(select_active_player(&players).unwrap().id, "playing");
        assert_eq!(select_active_player(&players[..2]).unwrap().id, "spotify");
        assert_eq!(select_active_player(&players[..1]).unwrap().id, "browser");
    }
}
