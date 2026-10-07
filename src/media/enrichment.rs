//! Session-local opt-in enrichment coordinator. Playback publication never waits
//! for HTTP. A completion is bound to the observed player owner and generation,
//! not Firefox's reusable track ID. No source URLs or lookup errors are logged.
use std::{
    collections::{HashMap, HashSet},
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

#[derive(Clone, PartialEq, Eq)]
struct Content {
    owner: String,
    source: Option<MediaSource>,
    title: String,
    artist: String,
    album: String,
    art_url: String,
    length_us: u64,
}
impl From<&MediaPlayer> for Content {
    fn from(player: &MediaPlayer) -> Self {
        Self {
            owner: player.owner.clone(),
            source: player.source.clone(),
            title: player.title.clone(),
            artist: player.artist.clone(),
            album: player.album.clone(),
            art_url: player.art_url.clone(),
            length_us: player.length_us,
        }
    }
}
struct Session {
    content: Content,
    generation: u64,
    // Keep displayed temporary artwork alive even after cache eviction.
    metadata: Option<Metadata>,
}
#[derive(Clone)]
struct Ticket {
    player: String,
    generation: u64,
}
pub(super) struct Completed {
    video: String,
    tickets: Vec<Ticket>,
    metadata: Option<Metadata>,
}
struct Cached {
    metadata: Option<Metadata>,
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
        if self.fetcher.is_none() {
            return;
        }
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
            let content = Content::from(&*player);
            if self
                .sessions
                .get(&player.id)
                .is_none_or(|session| session.content != content)
            {
                self.generation += 1;
                self.sessions.insert(
                    player.id.clone(),
                    Session {
                        content,
                        generation: self.generation,
                        metadata: None,
                    },
                );
            }
            let session = self.sessions.get_mut(&player.id).expect("observed session");
            // Unknown owners and unknown URLs cannot authorize a lookup.
            let video = player
                .source
                .as_ref()
                .and_then(|source| source::youtube_video_id(&source.url));
            let needed = player.title.trim().is_empty()
                || player.artist.trim().is_empty()
                || player.art_url.trim().is_empty();
            if let Some(video) = video.filter(|_| !player.owner.is_empty() && needed) {
                if let Some(entry) = self.cache.get_mut(&video) {
                    entry.used = now;
                    if let Some(metadata) = &entry.metadata {
                        session.metadata = Some(metadata.clone());
                    }
                } else if !self.pending.contains(&video) {
                    requests.entry(video).or_default().push(Ticket {
                        player: player.id.clone(),
                        generation: session.generation,
                    });
                }
            }
            if let Some(metadata) = &session.metadata {
                metadata.apply(player);
            }
        }
        for (video, tickets) in requests {
            if self.pending.len() >= MAX_REQUESTS || self.requests_started >= MAX_REQUESTS {
                break;
            }
            self.requests_started += 1;
            self.pending.insert(video.clone());
            let fetcher = self.fetcher.clone().expect("enabled fetcher");
            self.tasks.spawn(async move {
                // Deliberately do not log errors containing request URLs/video IDs.
                let metadata = fetcher.fetch(&video).await.ok();
                Completed {
                    video,
                    tickets,
                    metadata,
                }
            });
        }
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
                    metadata.apply(player);
                    session.metadata = Some(metadata.clone());
                }
            }
        }
        if self.cache.len() >= MAX_CACHE
            && let Some(oldest) = self
                .cache
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(id, _)| id.clone())
        {
            self.cache.remove(&oldest);
        }
        self.cache.insert(
            result.video,
            Cached {
                metadata: result.metadata,
                expires: now + ttl,
                used: now,
            },
        );
    }

    pub fn task_failed(&mut self) {
        // Retire all owned jobs on an unexpected worker panic; do not leave a
        // permanently pending ID or spin automatic retries.
        self.tasks.abort_all();
        let now = Instant::now();
        for video in self.pending.drain() {
            if self.cache.len() >= MAX_CACHE
                && let Some(oldest) = self
                    .cache
                    .iter()
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(id, _)| id.clone())
            {
                self.cache.remove(&oldest);
            }
            self.cache.insert(
                video,
                Cached {
                    metadata: None,
                    expires: now + FAILURE_TTL,
                    used: now,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests;
