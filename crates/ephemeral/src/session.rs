//! Session / presence keys.
//!
//! Presence is the "is this player/connection currently live?" signal. Like a
//! match snapshot it is ephemeral and TTL'd, but its lifecycle is a *heartbeat*:
//! a client refreshes ([`touch`](SessionStore::touch)) its presence key while
//! connected, and the key simply expires once the heartbeats stop, so a dropped
//! connection self-cleans without an explicit teardown.

use std::time::Duration;

use deadpool_redis::redis::AsyncCommands;
use deadpool_redis::Pool;

use crate::error::Result;
use crate::keys::Keys;

/// Reads and writes short-lived session/presence markers.
#[derive(Clone)]
pub struct SessionStore {
    pool: Pool,
    keys: Keys,
    default_ttl: Duration,
}

impl SessionStore {
    pub(crate) fn new(pool: Pool, keys: Keys, default_ttl: Duration) -> Self {
        Self {
            pool,
            keys,
            default_ttl,
        }
    }

    /// Set (or refresh) `session_id`'s presence marker to `value`, expiring after
    /// `ttl` (or the configured default). Called on each heartbeat: a live client
    /// keeps re-touching, and the key lapses once the heartbeats stop.
    pub async fn touch(&self, session_id: &str, value: &[u8], ttl: Option<Duration>) -> Result<()> {
        let ttl_secs = ttl.unwrap_or(self.default_ttl).as_secs().max(1);
        let mut conn = self.pool.get().await?;
        conn.set_ex::<_, _, ()>(self.keys.session(session_id), value, ttl_secs)
            .await?;
        Ok(())
    }

    /// Read `session_id`'s presence marker, or `None` if the session is not
    /// currently present (never touched, or its heartbeat lapsed).
    pub async fn read(&self, session_id: &str) -> Result<Option<Vec<u8>>> {
        let mut conn = self.pool.get().await?;
        let value: Option<Vec<u8>> = conn.get(self.keys.session(session_id)).await?;
        Ok(value)
    }

    /// Whether `session_id` is currently present.
    pub async fn is_present(&self, session_id: &str) -> Result<bool> {
        let mut conn = self.pool.get().await?;
        let exists: bool = conn.exists(self.keys.session(session_id)).await?;
        Ok(exists)
    }

    /// Explicitly end `session_id`'s presence (e.g. on a clean disconnect).
    /// Returns `true` if a marker was removed.
    pub async fn end(&self, session_id: &str) -> Result<bool> {
        let mut conn = self.pool.get().await?;
        let removed: i64 = conn.del(self.keys.session(session_id)).await?;
        Ok(removed > 0)
    }
}
