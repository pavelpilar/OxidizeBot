use crate::db::{self, models, schema};
use crate::track_id::TrackId;
use diesel::prelude::*;

pub use self::models::BlockedSong;

use chrono::Utc;

#[derive(Clone)]
pub struct BlockedSongs {
    db: db::Database,
}

impl BlockedSongs {
    /// Open the blocked songs database.
    pub async fn load(db: db::Database) -> Result<Self, anyhow::Error> {
        Ok(BlockedSongs { db })
    }

    /// Push the given Track ID.
    pub async fn push(&self, track_id: &TrackId, user: &str) -> Result<(), anyhow::Error> {
        use self::schema::blocked_songs::dsl;

        let track_id = track_id.clone();
        let user = user.to_string();

        self.db
            .asyncify(move |c| {
                let blocked_song = models::BlockedSong {
                    track_id,
                    blocked_by: String::from(user),
                    blocked_at: Utc::now().naive_utc(),
                };

                diesel::insert_into(dsl::blocked_songs)
                    .values(blocked_song)
                    .execute(c)?;
                Ok(())
            })
            .await
    }

    /// Delete the given Track ID.
    // TODO: Implement
    pub async fn delete(&self, track_id: &TrackId) -> Result<(), anyhow::Error> {
        Ok(())
    }

    /// List all available blocked songs.
    pub async fn list(&self) -> Result<Vec<BlockedSong>, anyhow::Error> {
        use self::schema::blocked_songs::dsl;

        self.db
            .asyncify(move |c| {
                Ok(dsl::blocked_songs
                    .order(dsl::blocked_at.asc())
                    .load::<models::BlockedSong>(c)?)
            })
            .await
    }

    pub async fn exists(&self, track_id: &TrackId) -> Result<bool, anyhow::Error> {
        use self::schema::blocked_songs::dsl;

        let track_id = track_id.clone();

        self.db
            .asyncify(move |c| {
                let song = dsl::blocked_songs
                    .filter(dsl::track_id.eq(&track_id))
                    .first::<models::BlockedSong>(c)
                    .optional()?;

                Ok(song.is_some())
            })
            .await
    }
}
