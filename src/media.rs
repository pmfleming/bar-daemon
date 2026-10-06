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
    model::{MediaContentType, MediaControlMode, MediaPlayer, MediaState},
    state::StateStore,
    time::unix_ms as unix_time_ms,
};

mod browser_identity;

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
const MPRIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);

// Session-local policy, serialized with publication. No wall-clock guesses and
// no player selection effects: pins and overrides never invoke MPRIS playback.
#[derive(Default)]
struct MediaSelection {
    pinned: Option<String>,
    previous: HashMap<String, bool>,
    started: HashMap<String, u64>,
    modes: HashMap<String, MediaControlMode>,
    sequence: u64,
    initialized: bool,
}

impl MediaSelection {
    fn active(&self, players: &[MediaPlayer]) -> Option<String> {
        if let Some(id) = &self.pinned {
            return Some(id.clone());
        }
        let recent = |playing_only: bool| {
            players
                .iter()
                .filter(|p| {
                    self.started.contains_key(&p.id)
                        && (!playing_only || p.playback_status == "playing")
                })
                .max_by_key(|p| self.started.get(&p.id).copied().unwrap_or_default())
        };
        recent(true)
            .or_else(|| players.iter().find(|p| p.playback_status == "playing"))
            .or_else(|| recent(false))
            .or_else(|| select_active_player(players))
            .map(|p| p.id.clone())
    }

    fn snapshot(&mut self, mut players: Vec<MediaPlayer>) -> MediaState {
        let present = |id: &String| players.iter().any(|p| &p.id == id);
        self.previous.retain(|id, _| present(id));
        self.started.retain(|id, _| present(id));
        self.modes.retain(|id, _| present(id));
        if self.pinned.as_ref().is_some_and(|id| !present(id)) {
            self.pinned = None;
        }
        for player in &mut players {
            let playing = player.playback_status == "playing";
            if self.initialized
                && playing
                && !self.previous.get(&player.id).copied().unwrap_or(false)
            {
                self.sequence = self.sequence.saturating_add(1);
                self.started.insert(player.id.clone(), self.sequence);
            }
            self.previous.insert(player.id.clone(), playing);
            player.control_mode = self.modes.get(&player.id).copied().unwrap_or_default();
        }
        self.initialized = !players.is_empty();
        MediaState {
            available: !players.is_empty(),
            active_player: self.active(&players),
            pinned_player: self.pinned.clone(),
            players,
            error: None,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct MediaService {
    selection: Arc<RwLock<MediaSelection>>,
    connection: Arc<RwLock<Option<zbus::Connection>>>,
}

impl MediaService {
    async fn connection(&self) -> Result<zbus::Connection> {
        if let Some(connection) = self.connection.read().await.clone() {
            return Ok(connection);
        }
        zbus::Connection::session()
            .await
            .context("connect to session D-Bus")
    }

    pub(crate) async fn seek(
        &self,
        player_id: Option<&str>,
        offset_seconds: i64,
    ) -> Result<String> {
        let offset_microseconds = seek_offset_microseconds(offset_seconds)?;
        let connection = self.connection().await?;
        let selected = resolve_player(&connection, player_id).await?;
        call_player(&connection, &selected, "Seek", &(offset_microseconds,)).await?;
        Ok(selected)
    }

    pub(crate) async fn operation(
        &self,
        player_id: Option<&str>,
        operation: &str,
    ) -> Result<String> {
        let method = operation_method(operation)?;
        let connection = self.connection().await?;
        let selected = resolve_player(&connection, player_id).await?;
        call_player(&connection, &selected, method, &()).await?;
        Ok(selected)
    }

    pub(crate) async fn cycle(&self, store: &StateStore) -> Result<MediaState> {
        let mut selection = self.selection.write().await;
        let current = store.read(|s| s.media.clone()).await;
        selection.pinned = Some(
            next_player_id(&current.players, current.active_player.as_deref())
                .context("no alternate MPRIS player is available")?,
        );
        let state = selection.snapshot(current.players);
        store.update_media(state.clone()).await;
        Ok(state)
    }

    pub(crate) async fn select(&self, store: &StateStore, id: Option<&str>) -> Result<MediaState> {
        let mut selection = self.selection.write().await;
        let players = store.read(|s| s.media.players.clone()).await;
        if id.is_some_and(|id| !players.iter().any(|p| p.id == id)) {
            bail!("requested media player is no longer available");
        }
        selection.pinned = id.map(str::to_owned);
        let state = selection.snapshot(players);
        store.update_media(state.clone()).await;
        Ok(state)
    }

    pub(crate) async fn set_mode(
        &self,
        store: &StateStore,
        id: &str,
        mode: MediaControlMode,
    ) -> Result<MediaState> {
        let mut selection = self.selection.write().await;
        let players = store.read(|s| s.media.players.clone()).await;
        if !players.iter().any(|p| p.id == id) {
            bail!("requested media player is no longer available");
        }
        if mode == MediaControlMode::Automatic {
            selection.modes.remove(id);
        } else {
            selection.modes.insert(id.to_owned(), mode);
        }
        let state = selection.snapshot(players);
        store.update_media(state.clone()).await;
        Ok(state)
    }

    async fn publish(&self, store: &StateStore, players: Vec<MediaPlayer>) {
        let mut selection = self.selection.write().await;
        store.update_media(selection.snapshot(players)).await;
    }
}

pub(crate) async fn monitor(store: StateStore, service: MediaService) {
    loop {
        match zbus::Connection::session().await {
            Ok(connection) => {
                *service.connection.write().await = Some(connection.clone());
                if let Err(error) = monitor_connection(&connection, &store, &service).await {
                    tracing::warn!(%error, "MPRIS monitor disconnected");
                }
                *service.connection.write().await = None;
                service.publish(&store, Vec::new()).await;
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
    let mut identity = root
        .get_property::<String>("Identity")
        .await
        .unwrap_or_else(|_| name.trim_start_matches(PREFIX).to_string());
    let mut desktop_entry = root
        .get_property::<String>("DesktopEntry")
        .await
        .unwrap_or_default();
    if let Some(labels) = browser_identity::read(connection, name).await {
        identity = labels.identity;
        desktop_entry = labels.desktop_entry;
    }
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
        content_type: classify_content(
            property_string(&metadata, "xesam:contentType").as_deref(),
            property_string(&metadata, "xesam:url").as_deref(),
        ),
        control_mode: MediaControlMode::Automatic,
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
        can_seek: player.get_property("CanSeek").await.unwrap_or(false),
        can_next: player.get_property("CanGoNext").await.unwrap_or(false),
        can_previous: player.get_property("CanGoPrevious").await.unwrap_or(false),
    })
}

fn classify_content(content_type: Option<&str>, url: Option<&str>) -> MediaContentType {
    let kind = content_type.unwrap_or("").to_lowercase();
    let url = url.unwrap_or("");
    if kind == "podcast"
        || url.starts_with("spotify:episode:")
        || url.starts_with("https://open.spotify.com/episode/")
    {
        MediaContentType::Podcast
    } else if kind == "video" || kind.starts_with("video/") {
        MediaContentType::Video
    } else if kind == "music"
        || url.starts_with("spotify:track:")
        || url.starts_with("https://open.spotify.com/track/")
    {
        MediaContentType::Music
    } else {
        // Audio MIME types and player identity cannot distinguish songs from podcasts.
        MediaContentType::Unknown
    }
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
    if let Some(id) = player_id {
        if !id.starts_with(PREFIX) {
            bail!("requested MPRIS player ID is invalid");
        }
        return Ok(id.to_string());
    }
    let names = player_names(connection).await?;
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

async fn call_player<B>(
    connection: &zbus::Connection,
    player_id: &str,
    method: &str,
    body: &B,
) -> Result<()>
where
    B: serde::ser::Serialize + zvariant::DynamicType,
{
    tokio::time::timeout(MPRIS_OPERATION_TIMEOUT, async {
        let proxy = zbus::Proxy::new(connection, player_id, PATH, PLAYER_INTERFACE).await?;
        proxy.call_method(method, body).await?;
        Ok::<_, zbus::Error>(())
    })
    .await
    .context("MPRIS operation timed out")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MediaService, seek_offset_microseconds};
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

    struct FakePlayer {
        can_seek: bool,
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl FakePlayer {
        #[zbus(property)]
        fn can_seek(&self) -> bool {
            self.can_seek
        }

        #[zbus(property)]
        fn metadata(&self) -> std::collections::HashMap<String, zvariant::OwnedValue> {
            use zvariant::Value;
            [
                ("xesam:title", Value::from("Chapter / episode")),
                ("xesam:artist", Value::from(vec!["Author / host"])),
                ("xesam:album", Value::from("Book / podcast")),
                ("mpris:artUrl", Value::from("https://example.com/art.jpg")),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value.try_to_owned().unwrap()))
            .collect()
        }
    }

    #[tokio::test]
    async fn publishes_mpris_seek_capability() {
        for (seconds, expected) in [
            (-15, Some(-15_000_000)),
            (30, Some(30_000_000)),
            (0, None),
            (86_401, None),
        ] {
            assert_eq!(seek_offset_microseconds(seconds).ok(), expected);
        }
        // An isolated peer connection avoids touching the user's media players.
        for can_seek in [false, true] {
            let (server, client) = tokio::net::UnixStream::pair().unwrap();
            let server = zbus::connection::Builder::unix_stream(server)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p()
                .serve_at(super::PATH, FakePlayer { can_seek })
                .unwrap()
                .build();
            let client = zbus::connection::Builder::unix_stream(client).p2p().build();
            let (_server, client) = tokio::try_join!(server, client).unwrap();
            let player = super::read_player(&client, "org.mpris.MediaPlayer2.test")
                .await
                .unwrap();
            assert_eq!(player.can_seek, can_seek);
            assert_eq!(serde_json::to_value(&player).unwrap()["can_seek"], can_seek);
            assert_eq!(player.id, "org.mpris.MediaPlayer2.test");
            assert_eq!(player.title, "Chapter / episode");
            assert_eq!(player.artist, "Author / host");
            assert_eq!(player.album, "Book / podcast");
            assert_eq!(player.art_url, "https://example.com/art.jpg");
        }
    }

    #[test]
    fn classification_is_conservative_and_modes_are_closed() {
        use crate::model::{MediaContentType as Content, MediaControlMode as Mode};
        assert_eq!(
            super::classify_content(Some("audio/mpeg"), None),
            Content::Unknown
        );
        assert_eq!(
            super::classify_content(Some("video/mp4"), None),
            Content::Video
        );
        assert_eq!(
            super::classify_content(None, Some("spotify:track:123")),
            Content::Music
        );
        assert_eq!(
            super::classify_content(None, Some("https://open.spotify.com/episode/123")),
            Content::Podcast
        );
        assert!(serde_json::from_str::<Mode>("\"invented\"").is_err());
        assert_eq!(
            serde_json::from_str::<Mode>("\"automatic\"").unwrap(),
            Mode::Automatic
        );
    }

    #[tokio::test]
    async fn recent_playback_pin_automatic_and_exit_are_independent_of_order() {
        let store = StateStore::default();
        let service = MediaService::default();
        let a = player("a", "paused", false, true);
        let b = player("b", "paused", false, true);
        service.publish(&store, vec![a.clone(), b.clone()]).await;
        let mut a = a;
        a.playback_status = "playing".into();
        service.publish(&store, vec![a.clone(), b.clone()]).await;
        assert_eq!(
            store.snapshot().await.media.active_player.as_deref(),
            Some("a")
        );
        let mut b = b;
        b.playback_status = "playing".into();
        service.publish(&store, vec![a.clone(), b.clone()]).await;
        service.publish(&store, vec![b.clone(), a.clone()]).await;
        assert_eq!(
            store.snapshot().await.media.active_player.as_deref(),
            Some("b")
        );
        service.select(&store, Some("a")).await.unwrap();
        service.publish(&store, vec![b.clone(), a.clone()]).await;
        assert_eq!(
            store.snapshot().await.media.active_player.as_deref(),
            Some("a")
        );
        assert_eq!(
            service
                .select(&store, None)
                .await
                .unwrap()
                .active_player
                .as_deref(),
            Some("b")
        );
        service.select(&store, Some("b")).await.unwrap();
        service.publish(&store, vec![a]).await;
        let state = store.snapshot().await.media;
        assert_eq!(state.active_player.as_deref(), Some("a"));
        assert!(state.pinned_player.is_none());
        assert!(service.select(&store, Some("b")).await.is_err());
        assert!(
            service.connection.read().await.is_none(),
            "policy operations never open a playback connection"
        );
    }

    #[tokio::test]
    async fn mode_overrides_are_per_player_and_expire_on_exit() {
        use crate::model::MediaControlMode as Mode;
        let store = StateStore::default();
        let service = MediaService::default();
        let players = vec![
            player("a", "paused", false, true),
            player("b", "paused", false, true),
        ];
        service.publish(&store, players.clone()).await;
        let state = service.set_mode(&store, "a", Mode::Tracks).await.unwrap();
        assert_eq!(state.players[0].control_mode, Mode::Tracks);
        assert_eq!(state.players[1].control_mode, Mode::Automatic);
        service.publish(&store, players.clone()).await;
        assert_eq!(
            store.snapshot().await.media.players[0].control_mode,
            Mode::Tracks
        );
        service
            .set_mode(&store, "a", Mode::Automatic)
            .await
            .unwrap();
        assert_eq!(
            store.snapshot().await.media.players[0].control_mode,
            Mode::Automatic
        );
        service.set_mode(&store, "a", Mode::Seek).await.unwrap();
        service.publish(&store, players[1..].to_vec()).await;
        assert!(service.set_mode(&store, "a", Mode::Tracks).await.is_err());
        service.publish(&store, players).await;
        assert_eq!(
            store.snapshot().await.media.players[0].control_mode,
            Mode::Automatic
        );
    }

    #[tokio::test]
    async fn manual_cycle_is_retained_by_monitor_selection() {
        let store = StateStore::default();
        let service = MediaService::default();
        let players = vec![
            player("browser", "paused", false, true),
            player("spotify", "paused", true, true),
        ];
        service.publish(&store, players[..1].to_vec()).await;
        assert_eq!(
            store.snapshot().await.media.active_player.as_deref(),
            Some("browser")
        );
        let mut playing = players.clone();
        playing.push(player("playing", "playing", false, true));
        service.publish(&store, playing).await;
        assert_eq!(
            store.snapshot().await.media.active_player.as_deref(),
            Some("playing")
        );
        service.publish(&store, players.clone()).await;
        assert_eq!(
            store.snapshot().await.media.active_player.as_deref(),
            Some("spotify")
        );

        let state = service.cycle(&store).await.unwrap();

        assert_eq!(state.active_player.as_deref(), Some("browser"));
        service.publish(&store, players).await;
        assert_eq!(
            store.snapshot().await.media.active_player.as_deref(),
            Some("browser")
        );
    }
}
