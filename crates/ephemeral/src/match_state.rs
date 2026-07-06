//! Live match-state snapshots with a configurable TTL.
//!
//! A running match's authoritative snapshot is *ephemeral*: it lives only for the
//! duration of the match and is safe to lose on the durable store's terms (the
//! sealed replay in PostgreSQL is the record of truth). So each snapshot is
//! written to a namespaced string key with an expiry, and re-read within the same
//! match lifecycle (acceptance criterion: *live match state can be written and
//! read back within the same match lifecycle with configurable TTL*).
//!
//! The snapshot payload is opaque bytes — the caller owns its encoding (the
//! `game-session` crate's serialized state, say) — so this adapter stays
//! decoupled from the match rules.

use std::time::Duration;

use deadpool_redis::redis::AsyncCommands;
use deadpool_redis::Pool;

use crate::error::Result;
use crate::keys::Keys;

/// Reads and writes live match snapshots to Redis with a TTL.
#[derive(Clone)]
pub struct MatchStateStore {
    pool: Pool,
    keys: Keys,
    default_ttl: Duration,
}

impl MatchStateStore {
    pub(crate) fn new(pool: Pool, keys: Keys, default_ttl: Duration) -> Self {
        Self {
            pool,
            keys,
            default_ttl,
        }
    }

    /// The default TTL applied by [`write_snapshot`](Self::write_snapshot) when
    /// no explicit TTL is passed.
    pub fn default_ttl(&self) -> Duration {
        self.default_ttl
    }

    /// Write `snapshot` for `match_id`, expiring after `ttl` (or the configured
    /// default when `ttl` is `None`). Overwrites any existing snapshot and resets
    /// the expiry.
    pub async fn write_snapshot(
        &self,
        match_id: &str,
        snapshot: &[u8],
        ttl: Option<Duration>,
    ) -> Result<()> {
        let ttl = ttl.unwrap_or(self.default_ttl);
        // SET EX takes whole seconds; clamp sub-second TTLs up to 1s so a live
        // snapshot never lands with a zero (== no) expiry.
        let ttl_secs = ttl.as_secs().max(1);
        let mut conn = self.pool.get().await?;
        conn.set_ex::<_, _, ()>(self.keys.match_state(match_id), snapshot, ttl_secs)
            .await?;
        Ok(())
    }

    /// Read the snapshot for `match_id`, or `None` if it never existed or has
    /// already expired.
    pub async fn read_snapshot(&self, match_id: &str) -> Result<Option<Vec<u8>>> {
        let mut conn = self.pool.get().await?;
        let value: Option<Vec<u8>> = conn.get(self.keys.match_state(match_id)).await?;
        Ok(value)
    }

    /// The seconds of TTL remaining on `match_id`'s snapshot: `Some(n)` while it
    /// lives, `None` once it has expired or was never written. Lets a caller
    /// confirm the configured expiry is in force.
    pub async fn ttl_seconds(&self, match_id: &str) -> Result<Option<u64>> {
        let mut conn = self.pool.get().await?;
        // Redis TTL: -2 = no such key, -1 = key exists but has no expiry.
        let ttl: i64 = conn.ttl(self.keys.match_state(match_id)).await?;
        Ok(if ttl >= 0 { Some(ttl as u64) } else { None })
    }

    /// Delete `match_id`'s snapshot (e.g. once the match has ended and been
    /// sealed durably). Returns `true` if a snapshot was removed.
    pub async fn clear(&self, match_id: &str) -> Result<bool> {
        let mut conn = self.pool.get().await?;
        let removed: i64 = conn.del(self.keys.match_state(match_id)).await?;
        Ok(removed > 0)
    }
}
