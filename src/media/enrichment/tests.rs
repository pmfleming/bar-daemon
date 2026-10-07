use super::*;

fn player(id: &str, video: &str) -> MediaPlayer {
    MediaPlayer {
        id: id.into(),
        owner: ":1.42".into(),
        playback_status: "playing".into(),
        can_control: true,
        can_play: true,
        can_pause: true,
        can_seek: true,
        source: super::super::source::resolve(
            Some(&format!("https://youtube.com/watch?v={video}")),
            None,
        )
        .source,
        ..MediaPlayer::default()
    }
}
fn enabled() -> Enrichment {
    Enrichment {
        fetcher: Some(Fetcher::new().unwrap()),
        ..Enrichment::default()
    }
}
fn result(state: &Enrichment, player: &MediaPlayer) -> Completed {
    Completed {
        video: source::youtube_video_id(&player.source.as_ref().unwrap().url).unwrap(),
        tickets: vec![Ticket {
            player: player.id.clone(),
            generation: state.sessions[&player.id].generation,
        }],
        metadata: Some(Metadata {
            title: "Provider title".into(),
            artist: "Channel".into(),
            artwork: Some(std::sync::Arc::new(tempfile::NamedTempFile::new().unwrap())),
        }),
    }
}

// These current-thread tests never yield to spawned fetches. JoinSet aborts them
// on drop: no test contacts YouTube or the user's D-Bus/media session.
#[tokio::test]
async fn disabled_is_offline_and_success_fills_only_missing_fields_with_provenance() {
    let raw = player("browser", "RQzh-xnLRlM");
    let mut players = vec![raw.clone()];
    let mut disabled = Enrichment::default();
    disabled.prepare(&mut players);
    assert!(disabled.tasks.is_empty());
    assert_eq!(players[0], raw);
    let mut state = enabled();
    players[0].title = "Browser title".into();
    let original = players[0].clone();
    state.prepare(&mut players);
    assert_eq!(state.tasks.len(), 1);
    assert_eq!(
        players[0], original,
        "immediate publication never waits for HTTP"
    );
    let completed = result(&state, &players[0]);
    let path = completed
        .metadata
        .as_ref()
        .unwrap()
        .artwork
        .as_ref()
        .unwrap()
        .path()
        .to_owned();
    state.complete(completed, &mut players);
    assert_eq!(players[0].title, "Browser title");
    assert_eq!(players[0].artist, "Channel");
    assert!(players[0].art_url.starts_with("file:"));
    assert_eq!(players[0].metadata_sources.len(), 2);
    assert_eq!(players[0].metadata_sources["artist"], "youtube-oembed");
    assert_eq!(players[0].source, raw.source);
    assert_eq!(players[0].id, raw.id);
    assert_eq!(players[0].playback_status, raw.playback_status);
    assert!(players[0].can_seek);
    state.cache.clear();
    assert!(path.exists(), "current artwork outlives cache eviction");
    drop(state);
    assert!(
        !path.exists(),
        "temporary artwork is not persisted as watch history"
    );

    let mut state = enabled();
    let mut supplied = original;
    supplied.artist = "Browser channel".into();
    supplied.art_url = "https://browser.example/cover.jpg".into();
    let before = supplied.clone();
    state.prepare(std::slice::from_mut(&mut supplied));
    assert!(state.tasks.is_empty(), "complete metadata needs no request");
    assert_eq!(supplied, before);
    let legacy = serde_json::to_value(&supplied).unwrap();
    assert!(legacy.get("owner").is_none());
    let mut legacy = legacy;
    legacy.as_object_mut().unwrap().remove("metadata_sources");
    assert!(
        serde_json::from_value::<MediaPlayer>(legacy)
            .unwrap()
            .metadata_sources
            .is_empty()
    );
}

#[tokio::test]
async fn late_results_cannot_follow_owner_content_generation_or_removal() {
    for change in ["url", "owner", "title", "clear", "removed", "aba"] {
        let original = player("browser", "RQzh-xnLRlM");
        let mut players = vec![original.clone()];
        let mut state = enabled();
        state.prepare(&mut players);
        let completed = result(&state, &players[0]);
        match change {
            "url" | "aba" => players[0] = player("browser", "abcdefghijk"),
            "owner" => players[0].owner = ":1.99".into(),
            "title" => players[0].title = "New browser metadata".into(),
            "clear" => players[0].source = None,
            "removed" => players.clear(),
            _ => unreachable!(),
        }
        state.prepare(&mut players);
        if change == "aba" {
            players = vec![original];
            state.prepare(&mut players);
        }
        state.complete(completed, &mut players);
        assert!(
            players
                .iter()
                .all(|p| p.art_url.is_empty() && p.artist.is_empty()),
            "{change}"
        );
    }
}

#[tokio::test]
async fn cache_deduplication_negative_ttl_and_request_budget_are_bounded() {
    let raw = player("browser", "RQzh-xnLRlM");
    let mut state = enabled();
    let mut players = vec![raw.clone(), player("other-browser", "RQzh-xnLRlM")];
    state.prepare(&mut players);
    assert_eq!(state.tasks.len(), 1, "one request per video ID");
    let mut completed = result(&state, &players[0]);
    completed.metadata = None;
    state.complete(completed, &mut players);
    let mut players = vec![raw.clone()];
    state.prepare(&mut players);
    assert!(
        state.pending.is_empty(),
        "failure is cached, not replayed on every position signal"
    );
    state.cache.get_mut("RQzh-xnLRlM").unwrap().expires = Instant::now();
    state.prepare(&mut players);
    assert_eq!(state.pending.len(), 1);
    let completed = result(&state, &players[0]);
    state.complete(completed, &mut players);
    let mut players = vec![raw];
    state.prepare(&mut players);
    assert_eq!(players[0].title, "Provider title");
    assert!(state.pending.is_empty());
    let mut state = enabled();
    let mut many: Vec<_> = (0..100)
        .map(|n| player(&format!("player-{n}"), &format!("{n:011}")))
        .collect();
    state.prepare(&mut many);
    assert_eq!(state.pending.len(), MAX_REQUESTS);
    for n in 0..100 {
        state.complete(
            Completed {
                video: format!("{n:011}"),
                tickets: vec![],
                metadata: None,
            },
            &mut [],
        );
    }
    assert_eq!(state.cache.len(), MAX_CACHE);
    let started = state.tasks.len();
    state.prepare(&mut many);
    assert_eq!(
        state.tasks.len(),
        started,
        "global budget prevents cache churn causing a lookup storm"
    );
    state.task_failed();
    assert!(state.pending.is_empty());
    assert!(state.cache.len() <= MAX_CACHE);
}
