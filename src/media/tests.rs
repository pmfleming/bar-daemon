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

struct FakeBrowserRoot;

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl FakeBrowserRoot {
    #[zbus(property)]
    fn identity(&self) -> &str {
        "Mozilla zen"
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "zen"
    }
}

struct FakePlayer {
    can_seek: bool,
    url: Option<&'static str>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl FakePlayer {
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.can_seek
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        0.0 // Zen can report this while paused; do not invent a playback rate.
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        861_000_000
    }

    #[zbus(property)]
    fn metadata(&self) -> std::collections::HashMap<String, zvariant::OwnedValue> {
        use zvariant::Value;
        let mut entries = vec![
            ("xesam:title", Value::from("Chapter / episode")),
            ("xesam:artist", Value::from(vec!["Author / host"])),
            ("xesam:album", Value::from("Book / podcast")),
            ("mpris:artUrl", Value::from("https://example.com/art.jpg")),
            ("mpris:length", Value::from(2_589_000_000_i64)),
            (
                "mpris:trackid",
                Value::from(
                    zvariant::ObjectPath::try_from("/org/mpris/MediaPlayer2/firefox").unwrap(),
                ),
            ),
        ];
        if let Some(url) = self.url {
            entries.push(("xesam:url", Value::from(url)));
        }
        entries
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
        let (_server, client) = crate::test_support::dbus_peer(|builder| {
            builder.serve_at(
                super::PATH,
                FakePlayer {
                    can_seek,
                    url: None,
                },
            )
        })
        .await
        .unwrap();
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

#[tokio::test]
async fn source_changes_follow_metadata_not_the_reused_firefox_track_id() {
    use crate::model::{MediaContentType as Kind, MediaSourceService as Service};
    let (server, client) = crate::test_support::dbus_peer(|builder| {
        builder
            .serve_at(
                super::PATH,
                FakePlayer {
                    can_seek: true,
                    url: None,
                },
            )?
            .serve_at(super::PATH, FakeBrowserRoot)
    })
    .await
    .unwrap();
    let interface = server
        .object_server()
        .interface::<_, FakePlayer>(super::PATH)
        .await
        .unwrap();
    let name = "org.mpris.MediaPlayer2.firefox.instance_1_380";
    for (url, service, kind) in [
        (
            Some("https://www.youtube.com/watch?v=RQzh-xnLRlM"),
            Some(Service::Youtube),
            Kind::Video,
        ),
        (
            Some("https://soundcloud.com/artist/recording"),
            Some(Service::Soundcloud),
            Kind::Unknown,
        ),
        (Some("https://example.org/player"), None, Kind::Unknown),
        (None, None, Kind::Unknown),
    ] {
        interface.get_mut().await.url = url;
        let player = super::read_player(&client, name).await.unwrap();
        assert_eq!(
            player.source.as_ref().map(|source| source.url.as_str()),
            url
        );
        assert_eq!(
            player.source.as_ref().and_then(|source| source.service),
            service
        );
        assert_eq!(player.content_type, kind);
        assert_eq!(player.id, name);
        assert_eq!(player.identity, "Mozilla zen");
        assert_eq!(player.desktop_entry, "zen");
        assert_eq!(player.title, "Chapter / episode");
        assert_eq!(player.artist, "Author / host");
        assert_eq!(player.album, "Book / podcast");
        assert_eq!(player.art_url, "https://example.com/art.jpg");
        assert_eq!(player.length_us, 2_589_000_000);
        assert_eq!(player.position_us, 861_000_000);
        assert_eq!(player.playback_rate, 0.0);
        assert!(player.can_seek);
        // No transport methods exist on the fake: enrichment only reads.
        assert!(!player.can_next);
    }
}

#[test]
fn modes_are_closed() {
    use crate::model::MediaControlMode as Mode;
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
