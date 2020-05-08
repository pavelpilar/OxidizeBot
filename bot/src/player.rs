use crate::{
    api, bus, db, injector, prelude::*, settings, song_file::SongFile, spotify_id::SpotifyId,
    track_id::TrackId, utils,
};

pub(self) use self::{
    connect::{ConnectDevice, ConnectPlayer, ConnectStream},
    mixer::Mixer,
    playback_future::PlaybackFuture,
    player_internal::PlayerInternal,
    queue::Queue,
    youtube::YouTubePlayer,
};
pub use self::{item::Item, song::Song, track::Track};
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tracing::trace_span;
use tracing_futures::Instrument as _;

mod connect;
mod item;
mod mixer;
mod playback_future;
mod player_internal;
mod queue;
mod song;
mod track;
mod youtube;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Playing,
    Paused,
    // initial undefined state.
    None,
}

impl Default for State {
    fn default() -> Self {
        Self::None
    }
}

/// Event used by player integrations.
#[derive(Debug)]
pub enum IntegrationEvent {
    /// Indicate that the current device changed.
    DeviceChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerKind {
    Spotify,
    YouTube,
    None,
}

/// The source of action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// Event was generated automatically, don't broadcast feedback.
    Automatic,
    /// Event was generated from user input. Broadcast feedback.
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(self) enum PlaybackMode {
    /// The default playback mode.
    #[serde(rename = "default")]
    Default,
    /// Enqueue the next song instead of playing it.
    ///
    /// Only valid for the Spotify player.
    #[serde(rename = "queue")]
    Queue,
}

impl Default for PlaybackMode {
    fn default() -> Self {
        Self::Default
    }
}

/// A volume modification.
pub enum ModifyVolume {
    Increase(u32),
    Decrease(u32),
    Set(u32),
}

impl ModifyVolume {
    /// Apply the given modification.
    pub(self) fn apply(self, v: u32) -> u32 {
        use self::ModifyVolume::*;

        let v = match self {
            Increase(n) => v.saturating_add(n),
            Decrease(n) => v.saturating_sub(n),
            Set(v) => v,
        };

        u32::min(100, v)
    }
}

/// Converts a track into an Item.
///
/// Returns `None` if the service required to convert the item is not
/// authenticated.
pub(self) async fn convert_item(
    spotify: &api::Spotify,
    youtube: &api::YouTube,
    user: Option<&str>,
    track_id: &TrackId,
    duration_override: Option<Duration>,
) -> Result<Option<Item>> {
    let (track, duration) = match track_id {
        TrackId::Spotify(id) => {
            if !spotify.token.is_ready().await {
                return Ok(None);
            }

            let track_id_string = id.to_base62();
            let track = spotify.track(track_id_string).await?;
            let duration = Duration::from_millis(track.duration_ms.into());

            (Track::Spotify { track }, duration)
        }
        TrackId::YouTube(id) => {
            if !youtube.token.is_ready().await {
                return Ok(None);
            }

            let video = youtube.videos_by_id(id, "contentDetails,snippet").await?;

            let video = match video {
                Some(video) => video,
                None => bail!("no video found for id `{}`", id),
            };

            let content_details = video
                .content_details
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("video does not have content details"))?;

            let duration = str::parse::<utils::PtDuration>(&content_details.duration)?;
            (Track::YouTube { video }, duration.into_std())
        }
    };

    let duration = match duration_override {
        Some(duration) => duration,
        None => duration,
    };

    Ok(Some(Item {
        track_id: track_id.clone(),
        track,
        user: user.map(|user| user.to_string()),
        duration,
    }))
}

/// Run the player.
pub async fn run(
    injector: injector::Injector,
    db: db::Database,
    spotify: Arc<api::Spotify>,
    youtube: Arc<api::YouTube>,
    global_bus: Arc<bus::Bus<bus::Global>>,
    youtube_bus: Arc<bus::Bus<bus::YouTube>>,
    settings: settings::Settings,
) -> Result<(Player, impl Future<Output = Result<()>>)> {
    let settings = settings.scoped("player");

    let mut futures = utils::Futures::default();

    let (connect_stream, connect_player, device, future) =
        self::connect::setup(spotify.clone(), settings.scoped("spotify")).await?;

    futures.push(
        future
            .instrument(trace_span!(target: "futures", "spotify"))
            .boxed(),
    );

    let (youtube_player, future) =
        self::youtube::setup(youtube_bus, settings.scoped("youtube")).await?;

    futures.push(
        future
            .instrument(trace_span!(target: "futures", "youtube"))
            .boxed(),
    );

    let bus = bus::Bus::new();
    let queue = Queue::new(db.clone());

    let (song_update_interval_stream, song_update_interval) = settings
        .stream("song-update-interval")
        .or_with(utils::Duration::seconds(1))
        .await?;

    let song_update_interval = if song_update_interval.is_empty() {
        None
    } else {
        Some(tokio::time::interval(song_update_interval.as_std()))
    };

    let (detached_stream, detached) = settings.stream("detached").or_default().await?;

    let duplicate_duration = settings
        .var("duplicate-duration", utils::Duration::default())
        .await?;
    let song_switch_feedback = settings.var("song-switch-feedback", true).await?;
    let max_songs_per_user = settings.var("max-songs-per-user", 2).await?;
    let max_queue_length = settings.var("max-queue-length", 30).await?;

    let mixer = Mixer::new(queue.clone());

    let (playback_mode_stream, playback_mode) = settings
        .stream("playback-mode")
        .or_with_else(PlaybackMode::default)
        .await?;

    let internal = Arc::new(RwLock::new(PlayerInternal {
        injector: injector.clone(),
        player: PlayerKind::None,
        detached,
        spotify: spotify.clone(),
        youtube: youtube.clone(),
        connect_player: connect_player.clone(),
        youtube_player,
        playback_mode,
        mixer,
        bus,
        global_bus,
        song_switch_feedback,

        device,
        max_queue_length,
        max_songs_per_user,
        duplicate_duration,

        themes: injector.var().await?,
        closed: None,
    }));

    let parent_player = Player {
        inner: internal.clone(),
    };

    // future to initialize the player future.
    // Yeah, I know....
    let future = async move {
        // TODO: Move somewhere else and we no longer need to keep Queue storage
        // behind its own lock.
        {
            // Add tracks from database.
            for song in db.player_list().await? {
                let item = convert_item(
                    &*spotify,
                    &*youtube,
                    song.user.as_deref(),
                    &song.track_id,
                    None,
                )
                .await;

                if let Ok(Some(item)) = item {
                    queue.push_back_queue(Arc::new(item)).await;
                } else {
                    log::warn!("failed to convert db item: {:?}", song);
                }
            }
        }

        let playback = PlaybackFuture {
            internal: internal.clone(),
            connect_stream,
            playback_mode_stream,
            detached_stream,
            song_update_interval,
            song_update_interval_stream,
        };

        futures.push(
            SongFile::run(injector.clone(), settings.scoped("song-file"))
                .instrument(trace_span!(target: "futures", "song-file"))
                .boxed(),
        );

        futures.push(
            playback
                .run(injector.clone(), settings)
                .instrument(trace_span!(target: "futures", "playback"))
                .boxed(),
        );

        futures.select_next_some().await?;
        Ok(())
    };

    Ok((parent_player, future.boxed()))
}

/// Events emitted by the player.
#[derive(Debug, Clone)]
pub enum Event {
    /// Player is empty.
    Empty,
    /// Player is playing a song. If the song is known, it's provided.
    Playing(bool, Option<Arc<Item>>),
    /// The current song was skipped, and we don't know which song is playing
    /// next.
    Skip,
    /// Player is pausing.
    Pausing,
    /// queue was modified in some way.
    Modified,
    /// player has not been configured.
    NotConfigured,
    /// Player is detached.
    Detached,
}

/// All parts of a Player that can be shared between threads.
#[derive(Clone)]
pub struct Player {
    /// Player internals. Wrapped to make cloning cheaper since Player is frequently shared.
    inner: Arc<RwLock<PlayerInternal>>,
}

impl Player {
    /// Get a receiver for player events.
    pub async fn add_rx(&self) -> bus::Reader<Event> {
        self.inner.read().await.bus.add_rx()
    }

    /// Get the current device.
    pub async fn current_device(&self) -> Option<String> {
        let inner = self.inner.read().await;
        inner.device.current_device().await
    }

    /// List all available devices.
    pub async fn list_devices(&self) -> Result<Vec<api::spotify::Device>> {
        let inner = self.inner.read().await;
        inner.device.list_devices().await
    }

    /// External call to set device.
    ///
    /// Should always notify the player to change.
    pub async fn set_device(&self, device: String) -> Result<()> {
        let inner = self.inner.read().await;
        inner.device.set_device(Some(device)).await
    }

    /// Clear the current device.
    pub async fn clear_device(&self) -> Result<()> {
        let inner = self.inner.read().await;
        inner.device.set_device(None).await
    }

    /// Get the next N songs in queue.
    pub async fn list(&self) -> Vec<Arc<Item>> {
        let inner = self.inner.read().await;
        let queue = inner.mixer.queue.queue.read().await;
        let song = inner.injector.get::<Song>().await;

        song.as_ref()
            .map(|c| c.item.clone())
            .into_iter()
            .chain(queue.iter().cloned())
            .collect()
    }

    /// Promote the given song to the head of the queue.
    pub async fn promote_song(&self, user: Option<&str>, n: usize) -> Result<Option<Arc<Item>>> {
        let mut inner = self.inner.write().await;
        let promoted = inner.mixer.queue.promote_song(user, n).await?;

        if promoted.is_some() {
            inner.modified(Source::Manual).await?;
        }

        Ok(promoted)
    }

    /// Toggle playback.
    pub async fn toggle(&self) -> Result<()> {
        let mut inner = self.inner.write().await;
        inner.toggle(Source::Manual).await?;
        Ok(())
    }

    /// Start playback.
    pub async fn play(&self) -> Result<()> {
        let mut inner = self.inner.write().await;
        inner.play(Source::Manual).await?;
        Ok(())
    }

    /// Pause playback.
    pub async fn pause(&self) -> Result<()> {
        let mut inner = self.inner.write().await;
        inner.pause(Source::Manual).await?;
        Ok(())
    }

    /// Skip the current song.
    pub async fn skip(&self) -> Result<()> {
        let mut inner = self.inner.write().await;
        inner.skip(Source::Manual).await?;
        Ok(())
    }

    /// Update volume of the player.
    pub async fn volume(&self, modify: ModifyVolume) -> Result<Option<u32>> {
        let inner = self.inner.read().await;

        let track_id = match inner.injector.get::<Song>().await {
            Some(song) => song.item.track_id.clone(),
            None => {
                return Ok(None);
            }
        };

        Ok(match track_id {
            TrackId::Spotify(..) => Some(inner.connect_player.volume(modify).await?),
            TrackId::YouTube(..) => Some(inner.youtube_player.volume(modify).await?),
        })
    }

    /// Get the current volume.
    pub async fn current_volume(&self) -> Option<u32> {
        let inner = self.inner.read().await;

        let track_id = inner
            .injector
            .get::<Song>()
            .await
            .as_ref()?
            .item
            .track_id
            .clone();

        match track_id {
            TrackId::Spotify(..) => Some(inner.connect_player.current_volume().await),
            TrackId::YouTube(..) => Some(inner.youtube_player.current_volume().await),
        }
    }

    /// Close the player from more requests.
    pub async fn close(&self, reason: Option<String>) {
        self.inner.write().await.closed = Some(reason.map(Arc::new));
    }

    /// Open the player.
    pub async fn open(&self) {
        self.inner.write().await.closed = None;
    }

    /// Search for a track.
    pub async fn search_track(&self, q: &str) -> Result<Option<TrackId>> {
        let inner = self.inner.read().await;

        if q.starts_with("youtube:") {
            let q = q.trim_start_matches("youtube:");
            let results = inner.youtube.search(q).await?;

            let result = results.items.into_iter().filter(|r| match r.id.kind {
                api::youtube::Kind::Video => true,
                _ => false,
            });

            let mut result = result.flat_map(|r| r.id.video_id);
            return Ok(result.next().map(TrackId::YouTube));
        }

        let q = if q.starts_with("spotify:") {
            q.trim_start_matches("spotify:")
        } else {
            q
        };

        let page = inner.spotify.search_track(q).await?;

        match page.items.into_iter().next().and_then(|t| t.id) {
            Some(track_id) => match SpotifyId::from_base62(&track_id) {
                Ok(track_id) => Ok(Some(TrackId::Spotify(track_id))),
                Err(_) => bail!("search result returned malformed id"),
            },
            None => Ok(None),
        }
    }

    /// Play a theme track.
    pub async fn play_theme(&self, channel: &str, name: &str) -> Result<(), PlayThemeError> {
        let mut inner = self.inner.write().await;

        let themes = match inner.themes.load().await {
            Some(themes) => themes,
            None => return Err(PlayThemeError::NotConfigured),
        };

        let theme = match themes.get(channel, name).await {
            Some(theme) => theme,
            None => return Err(PlayThemeError::NoSuchTheme),
        };

        let duration = theme.end.clone().map(|o| o.as_duration());

        let item = convert_item(
            &*inner.spotify,
            &*inner.youtube,
            None,
            &theme.track_id,
            duration,
        )
        .await
        .map_err(PlayThemeError::Error)?;

        let item = match item {
            Some(item) => item,
            None => return Err(PlayThemeError::MissingAuth),
        };

        let item = Arc::new(item);
        let duration = theme.start.as_duration();

        inner
            .inject(Source::Manual, item, duration)
            .await
            .map_err(PlayThemeError::Error)?;
        Ok(())
    }

    /// Add the given track to the queue.
    ///
    /// Returns the item added.
    pub async fn add_track(
        &self,
        user: &str,
        track_id: TrackId,
        bypass_constraints: bool,
        max_duration: Option<utils::Duration>,
    ) -> Result<(Option<usize>, Arc<Item>), AddTrackError> {
        let mut inner = self.inner.write().await;
        inner
            .add_track(user, track_id, bypass_constraints, max_duration)
            .await
    }

    pub async fn purge(&self) -> Result<Vec<Arc<Item>>> {
        let mut inner = self.inner.write().await;
        let purged = inner.mixer.queue.purge().await?;

        if !purged.is_empty() {
            inner.modified(Source::Manual).await?;
        }

        Ok(purged)
    }

    /// Remove the item at the given position.
    pub async fn remove_at(&self, n: usize) -> Result<Option<Arc<Item>>> {
        let mut inner = self.inner.write().await;
        let removed = inner.mixer.queue.remove_at(n).await?;

        if removed.is_some() {
            inner.modified(Source::Manual).await?;
        }

        Ok(removed)
    }

    /// Remove the first track in the queue.
    pub async fn remove_last(&self) -> Result<Option<Arc<Item>>> {
        let mut inner = self.inner.write().await;
        let removed = inner.mixer.queue.remove_last().await?;

        if removed.is_some() {
            inner.modified(Source::Manual).await?;
        }

        Ok(removed)
    }

    /// Remove the last track by the given user.
    pub async fn remove_last_by_user(&self, user: &str) -> Result<Option<Arc<Item>>> {
        let mut inner = self.inner.write().await;
        let removed = inner.mixer.queue.remove_last_by_user(user).await?;

        if removed.is_some() {
            inner.modified(Source::Manual).await?;
        }

        Ok(removed)
    }

    /// Find the next item that matches the given predicate and how long until it plays.
    pub async fn find(
        &self,
        mut predicate: impl FnMut(&Item) -> bool,
    ) -> Option<(Duration, Arc<Item>)> {
        let inner = self.inner.read().await;

        let mut duration = Duration::default();

        if let Some(c) = inner.injector.get::<Song>().await {
            if predicate(&c.item) {
                return Some((Default::default(), c.item.clone()));
            }

            duration += c.remaining();
        }

        let queue = inner.mixer.queue.queue.read().await;

        for item in &*queue {
            if predicate(item) {
                return Some((duration, item.clone()));
            }

            duration += item.duration;
        }

        None
    }

    /// Get the length in number of items and total number of seconds in queue.
    pub async fn length(&self) -> (usize, Duration) {
        let inner = self.inner.read().await;

        let mut count = 0;
        let mut duration = Duration::default();

        if let Some(song) = inner.injector.get::<Song>().await.as_ref() {
            duration += song.remaining();
            count += 1;
        }

        let queue = inner.mixer.queue.queue.read().await;

        for item in &*queue {
            duration += item.duration;
        }

        count += queue.len();
        (count, duration)
    }

    /// Get the current song, if it is set.
    pub async fn current(&self) -> Option<Song> {
        self.inner.read().await.injector.get::<Song>().await
    }
}

/// Error raised when failing to play a theme song.
pub enum PlayThemeError {
    /// No such theme song.
    NoSuchTheme,
    /// Themes system is not configured.
    NotConfigured,
    /// Authentication missing to play the given theme.
    MissingAuth,
    /// Other generic error happened.
    Error(anyhow::Error),
}

/// Error raised when trying to add track.
pub enum AddTrackError {
    /// Queue is full.
    QueueFull,
    /// Queue already contains track.
    QueueContainsTrack(usize),
    /// Too many user tracks.
    TooManyUserTracks(u32),
    /// Player has been closed from adding more tracks to the queue with an optional reason.
    PlayerClosed(Option<Arc<String>>),
    /// Duplicate song that was added at the specified time by the specified user.
    Duplicate(DateTime<Utc>, Option<String>, Duration),
    /// Authentication missing for adding the given track.
    MissingAuth,
    /// Playback mode is not supported for the given track.
    UnsupportedPlaybackMode,
    /// Other generic error happened.
    Error(anyhow::Error),
}
