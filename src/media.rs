use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tokio::{sync::mpsc, task::JoinSet, time::sleep};
use zvariant::OwnedValue;

use crate::{
    model::{MediaPlayer, MediaState},
    state::StateStore,
};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

pub async fn monitor(store: StateStore) {
    loop {
        match zbus::Connection::session().await {
            Ok(connection) => {
                if let Err(error) = monitor_connection(&connection, &store).await {
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

async fn monitor_connection(connection: &zbus::Connection, store: &StateStore) -> Result<()> {
    let dbus = zbus::fdo::DBusProxy::new(connection).await?;
    let mut owner_changes = dbus.receive_name_owner_changed().await?;
    let (changes_tx, mut changes_rx) = mpsc::channel::<()>(32);
    let mut watchers = JoinSet::new();
    let mut names = Vec::new();

    refresh(connection, store, &mut names, &mut watchers, &changes_tx).await;
    loop {
        tokio::select! {
            signal = owner_changes.next() => {
                let Some(signal) = signal else { bail!("D-Bus owner-change stream ended"); };
                let args = signal.args()?;
                if args.name().as_str().starts_with(PREFIX) {
                    refresh(connection, store, &mut names, &mut watchers, &changes_tx).await;
                }
            }
            changed = changes_rx.recv() => {
                if changed.is_none() { bail!("MPRIS property watcher ended"); }
                refresh_players(connection, store, &names).await;
            }
        }
    }
}

async fn refresh(
    connection: &zbus::Connection,
    store: &StateStore,
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
            refresh_players(connection, store, watched_names).await;
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

async fn refresh_players(connection: &zbus::Connection, store: &StateStore, names: &[String]) {
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
    let active_player = select_active_player(&players).map(|player| player.id.clone());
    store
        .update_media(MediaState {
            available: !players.is_empty(),
            active_player,
            players,
            error: None,
        })
        .await;
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

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn property_strings(values: &HashMap<String, OwnedValue>, key: &str) -> Vec<String> {
    values
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<String>::try_from(value).ok())
        .unwrap_or_default()
}

pub fn select_active_player(players: &[MediaPlayer]) -> Option<&MediaPlayer> {
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

pub async fn operation(player_id: Option<&str>, operation: &str) -> Result<String> {
    let method = operation_method(operation)?;
    let connection = zbus::Connection::session()
        .await
        .context("connect to session D-Bus")?;
    let names = player_names(&connection).await?;
    let selected = if let Some(id) = player_id {
        if !names.iter().any(|name| name == id) {
            bail!("requested MPRIS player is unavailable");
        }
        id.to_string()
    } else {
        let mut players = Vec::new();
        for name in &names {
            if let Ok(player) = read_player(&connection, name).await {
                players.push(player);
            }
        }
        select_active_player(&players)
            .map(|player| player.id.clone())
            .context("no MPRIS player is available")?
    };
    let proxy = zbus::Proxy::new(&connection, selected.as_str(), PATH, PLAYER_INTERFACE).await?;
    proxy
        .call_method(method, &())
        .await
        .context("call MPRIS player operation")?;
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::{operation_method, select_active_player};
    use crate::model::MediaPlayer;

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
