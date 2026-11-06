//! Session-local opt-in enrichment coordinator. Playback publication never waits
//! for HTTP. A completion is bound to the observed player owner and generation,
//! not Firefox's reusable track ID. No source URLs or lookup errors are logged.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::task::JoinSet;

use super::{
    source,
    youtube::{Fetcher, Metadata},
};
use crate::model::{MediaPlayer, MediaSource};

const MAX_REQUESTS: usize = 4;
const MAX_CACHE: usize = 32;
const SUCCESS_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const FAILURE_TTL: Duration = Duration::from_secs(5 * 60);

// Compare borrowed content, not timing/capability updates. Retain a raw snapshot
// only when content changes rather than copying all its strings on every signal.
fn content_key(player: &MediaPlayer) -> ([&str; 5], Option<&MediaSource>, u64) {
    (
        [
            &player.owner,
            &player.title,
            &player.artist,
            &player.album,
            &player.art_url,
        ],
        player.source.as_ref(),
        player.length_us,
    )
}
#[derive(Default)]
struct Session {
    observed: MediaPlayer,
    generation: u64,
    // Keep displayed temporary artwork alive even after cache eviction.
    metadata: Option<Arc<Metadata>>,
}
struct Ticket {
    player: String,
    generation: u64,
}
pub(super) struct Completed {
    video: String,
    tickets: Vec<Ticket>,
    metadata: Option<Arc<Metadata>>,
}
struct Cached {
    metadata: Option<Arc<Metadata>>,
    expires: Instant,
    used: Instant,
}

#[derive(Default)]
pub(super) struct Enrichment {
    fetcher: Option<Fetcher>,
    sessions: HashMap<String, Session>,
    generation: u64,
    cache: HashMap<String, Cached>,
    pending: HashSet<String>,
    request_window: Option<Instant>,
    requests_started: usize,
    pub tasks: JoinSet<Completed>,
}

impl Enrichment {
    pub fn from_environment() -> Self {
        let enabled = std::env::var("BAR_DAEMON_YOUTUBE_METADATA")
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
        Self {
            fetcher: enabled.then(Fetcher::new).and_then(Result::ok),
            ..Self::default()
        }
    }

    pub fn prepare(&mut self, players: &mut [MediaPlayer]) {
        let Some(fetcher) = self.fetcher.clone() else {
            return;
        };
        let now = Instant::now();
        if self
            .request_window
            .is_none_or(|start| now.duration_since(start) >= Duration::from_secs(10))
        {
            self.request_window = Some(now);
            self.requests_started = 0;
        }
        self.cache.retain(|_, entry| entry.expires > now);
        self.sessions
            .retain(|id, _| players.iter().any(|player| &player.id == id));
        let mut requests: HashMap<String, Vec<Ticket>> = HashMap::new();
        for player in players {
            if let Some((video, ticket)) = self.prepare_player(player, now) {
                requests.entry(video).or_default().push(ticket);
            }
        }
        for (video, tickets) in requests {
            if self.pending.len() >= MAX_REQUESTS || self.requests_started >= MAX_REQUESTS {
                break;
            }
            self.requests_started += 1;
            self.pending.insert(video.clone());
            let fetcher = fetcher.clone();
            self.tasks.spawn(async move {
                // Deliberately do not log errors containing request URLs/video IDs.
                let metadata = fetcher.fetch(&video).await.ok().map(Arc::new);
                Completed {
                    video,
                    tickets,
                    metadata,
                }
            });
        }
    }

    fn prepare_player(
        &mut self,
        player: &mut MediaPlayer,
        now: Instant,
    ) -> Option<(String, Ticket)> {
        let session = self.sessions.entry(player.id.clone()).or_default();
        if session.generation == 0 || content_key(&session.observed) != content_key(player) {
            self.generation += 1;
            *session = Session {
                observed: player.clone(),
                generation: self.generation,
                metadata: None,
            };
        }
        let mut request = None;
        if let Some(video) = lookup_video(player) {
            if let Some(entry) = self.cache.get_mut(&video) {
                entry.used = now;
                if let Some(metadata) = &entry.metadata {
                    session.metadata = Some(Arc::clone(metadata));
                }
            } else if !self.pending.contains(&video) {
                request = Some((
                    video,
                    Ticket {
                        player: player.id.clone(),
                        generation: session.generation,
                    },
                ));
            }
        }
        if let Some(metadata) = &session.metadata {
            apply_metadata(metadata, player);
        }
        request
    }

    // Called only after a fresh MPRIS read and prepare(), including owner lookup.
    pub fn complete(&mut self, result: Completed, players: &mut [MediaPlayer]) {
        self.pending.remove(&result.video);
        let now = Instant::now();
        let ttl = if result
            .metadata
            .as_ref()
            .is_some_and(|data| data.artwork.is_some())
        {
            SUCCESS_TTL
        } else {
            FAILURE_TTL
        };
        if let Some(metadata) = &result.metadata {
            for ticket in &result.tickets {
                if let Some(session) = self.sessions.get_mut(&ticket.player)
                    && session.generation == ticket.generation
                    && let Some(player) =
                        players.iter_mut().find(|player| player.id == ticket.player)
                {
                    apply_metadata(metadata, player);
                    session.metadata = Some(Arc::clone(metadata));
                }
            }
        }
        cache_result(&mut self.cache, result.video, result.metadata, now, ttl);
    }

    pub fn task_failed(&mut self) {
        // Retire all owned jobs on an unexpected worker panic; do not leave a
        // permanently pending ID or spin automatic retries.
        self.tasks.abort_all();
        let now = Instant::now();
        for video in self.pending.drain() {
            cache_result(&mut self.cache, video, None, now, FAILURE_TTL);
        }
    }
}

fn cache_result(
    cache: &mut HashMap<String, Cached>,
    video: String,
    metadata: Option<Arc<Metadata>>,
    now: Instant,
    ttl: Duration,
) {
    if cache.len() >= MAX_CACHE
        && let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.used)
            .map(|(id, _)| id.clone())
    {
        cache.remove(&oldest);
    }
    cache.insert(
        video,
        Cached {
            metadata,
            expires: now + ttl,
            used: now,
        },
    );
}

// Provider data is independent of MPRIS; only the coordinator owns publication.
fn apply_metadata(metadata: &Metadata, player: &mut MediaPlayer) {
    for (field, target, value) in [
        ("title", &mut player.title, &metadata.title),
        ("artist", &mut player.artist, &metadata.artist),
    ] {
        if target.trim().is_empty() && !value.is_empty() {
            target.clone_from(value);
            player
                .metadata_sources
                .insert(field.into(), "youtube-oembed".into());
        }
    }
    if player.art_url.trim().is_empty()
        && let Some(file) = &metadata.artwork
        && let Ok(url) = reqwest::Url::from_file_path(file.path())
    {
        player.art_url = url.to_string();
        player
            .metadata_sources
            .insert("art_url".into(), "youtube-oembed".into());
    }
}

fn lookup_video(player: &MediaPlayer) -> Option<String> {
    // Unknown owners and complete MPRIS metadata cannot authorize a lookup.
    if player.owner.is_empty()
        || [&player.title, &player.artist, &player.art_url]
            .iter()
            .all(|value| !value.trim().is_empty())
    {
        return None;
    }
    source::youtube_video_id(&player.source.as_ref()?.url)
}

#[cfg(test)]
mod tests;
