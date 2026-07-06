//! The dual-axis matchmaking queue.
//!
//! The matchmaking service pairs players on *two* axes: a primary MMR axis and a
//! secondary axis (e.g. level), mirroring the ±Rating / ±Level search bands the
//! [`MatchmakingTicket`](../../domain/matchmaking_ticket) aggregate enforces.
//! Redis has no native two-dimensional index, so this adapter composes two
//! structures per queue:
//!
//! * a **sorted set** (`…:mmr`) scored by MMR — the axis Redis can range-scan,
//! * a **hash** (`…:secondary`) holding each member's secondary value.
//!
//! A candidate lookup range-scans the MMR band with `ZRANGEBYSCORE`, then filters
//! the returned members by the secondary band in the adapter (the pure
//! [`within_secondary_band`] predicate). This satisfies the acceptance criterion:
//! *matchmaking queue supports enqueue, dequeue, and dual-axis (MMR + secondary)
//! candidate lookup*.

use deadpool_redis::redis::{cmd, AsyncCommands};
use deadpool_redis::Pool;

use crate::error::Result;
use crate::keys::Keys;

/// One enqueued matchmaking candidate: its id, primary MMR axis, and secondary
/// axis value.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The candidate's identity (ticket or player id) — the queue member.
    pub id: String,
    /// Primary axis: matchmaking rating. The sorted-set score.
    pub mmr: f64,
    /// Secondary axis: e.g. account level. Filtered in the adapter.
    pub secondary: i64,
}

impl Candidate {
    /// Build a candidate.
    pub fn new(id: impl Into<String>, mmr: f64, secondary: i64) -> Self {
        Self {
            id: id.into(),
            mmr,
            secondary,
        }
    }
}

/// The most queue members a single [`find_candidates`](MatchmakingQueue::find_candidates)
/// call will pull off the MMR axis per requested result, before the secondary
/// filter is applied — a spread wide enough to survive secondary-axis misses
/// without scanning the whole band.
const SECONDARY_HIT_MULTIPLIER: usize = 4;

/// A hard ceiling on the working set a single lookup will scan, so a pathological
/// `limit` or a huge queue can never make one call pull an unbounded band into
/// memory (a denial-of-service guard).
const MAX_CANDIDATE_SCAN: usize = 500;

/// Pure predicate: is `value` within `±band` of `target` on the secondary axis?
///
/// Extracted so the dual-axis filter is unit-testable without a live Redis (the
/// MMR axis is filtered by Redis itself via the `ZRANGEBYSCORE` bounds).
///
/// The subtraction is widened to `i128` so it cannot overflow (and panic) even at
/// the `i64` extremes; a negative `band` simply matches nothing.
pub fn within_secondary_band(value: i64, target: i64, band: i64) -> bool {
    (value as i128 - target as i128).abs() <= band as i128
}

/// A Redis-backed dual-axis matchmaking queue.
#[derive(Clone)]
pub struct MatchmakingQueue {
    pool: Pool,
    keys: Keys,
}

impl MatchmakingQueue {
    pub(crate) fn new(pool: Pool, keys: Keys) -> Self {
        Self { pool, keys }
    }

    /// Enqueue (or re-position) `candidate` in `queue_id`: record its MMR on the
    /// sorted set and its secondary value on the companion hash. Idempotent —
    /// enqueuing an existing member updates both axes.
    pub async fn enqueue(&self, queue_id: &str, candidate: &Candidate) -> Result<()> {
        let mut conn = self.pool.get().await?;
        conn.zadd::<_, _, _, ()>(self.keys.queue_mmr(queue_id), &candidate.id, candidate.mmr)
            .await?;
        conn.hset::<_, _, _, ()>(
            self.keys.queue_secondary(queue_id),
            &candidate.id,
            candidate.secondary,
        )
        .await?;
        Ok(())
    }

    /// Remove `id` from `queue_id` (both axes). Returns `true` if the member was
    /// present on the primary axis.
    pub async fn dequeue(&self, queue_id: &str, id: &str) -> Result<bool> {
        let mut conn = self.pool.get().await?;
        let removed: i64 = conn.zrem(self.keys.queue_mmr(queue_id), id).await?;
        conn.hdel::<_, _, ()>(self.keys.queue_secondary(queue_id), id)
            .await?;
        Ok(removed > 0)
    }

    /// The number of candidates currently queued in `queue_id`.
    pub async fn len(&self, queue_id: &str) -> Result<u64> {
        let mut conn = self.pool.get().await?;
        let n: u64 = conn.zcard(self.keys.queue_mmr(queue_id)).await?;
        Ok(n)
    }

    /// Dual-axis candidate lookup: return up to `limit` candidates in `queue_id`
    /// whose MMR is within `±mmr_band` of `target.mmr` **and** whose secondary
    /// value is within `±secondary_band` of `target.secondary`, excluding
    /// `target.id` itself.
    ///
    /// The MMR band is applied by Redis (`ZRANGEBYSCORE`); the secondary band is
    /// applied in-adapter via [`within_secondary_band`]. Results are ordered by
    /// ascending MMR (the sorted set's natural order).
    ///
    /// The scan is *bounded*: Redis returns at most
    /// `limit * SECONDARY_HIT_MULTIPLIER` members (capped by [`MAX_CANDIDATE_SCAN`])
    /// via `ZRANGEBYSCORE … LIMIT`, and their secondary values are fetched in a
    /// single `HMGET` — so one call never pulls an unbounded band, nor issues a
    /// round-trip per member.
    pub async fn find_candidates(
        &self,
        queue_id: &str,
        target: &Candidate,
        mmr_band: f64,
        secondary_band: i64,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut conn = self.pool.get().await?;

        let min = target.mmr - mmr_band;
        let max = target.mmr + mmr_band;
        // Bound the working set: enough headroom for secondary-axis misses, but
        // never more than the hard cap. `count` is the Redis LIMIT.
        let count = limit
            .saturating_mul(SECONDARY_HIT_MULTIPLIER)
            .clamp(1, MAX_CANDIDATE_SCAN);
        // Range-scan the MMR axis (bounded), carrying each member's score back.
        let scored: Vec<(String, f64)> = conn
            .zrangebyscore_limit_withscores(
                self.keys.queue_mmr(queue_id),
                min,
                max,
                0,
                count as isize,
            )
            .await?;

        // Drop the target itself; if nothing remains there is nothing to fetch
        // (an HMGET with zero fields is an error, so guard it).
        let scanned: Vec<(String, f64)> = scored
            .into_iter()
            .filter(|(id, _)| id != &target.id)
            .collect();
        if scanned.is_empty() {
            return Ok(Vec::new());
        }

        // One HMGET for every candidate's secondary axis, rather than N HGETs.
        // (`AsyncCommands::hget` is single-field only, so build HMGET directly.)
        let mut hmget = cmd("HMGET");
        hmget.arg(self.keys.queue_secondary(queue_id));
        for (id, _) in &scanned {
            hmget.arg(id);
        }
        let secondaries: Vec<Option<i64>> = hmget.query_async(&mut conn).await?;

        let mut out = Vec::new();
        for ((id, mmr), secondary) in scanned.into_iter().zip(secondaries) {
            let Some(secondary) = secondary else { continue };
            if within_secondary_band(secondary, target.secondary, secondary_band) {
                out.push(Candidate { id, mmr, secondary });
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_band_includes_the_edges() {
        // Within band (target 20, ±5): 15..=25 inclusive.
        assert!(within_secondary_band(15, 20, 5));
        assert!(within_secondary_band(25, 20, 5));
        assert!(within_secondary_band(20, 20, 5));
    }

    #[test]
    fn secondary_band_excludes_beyond_the_edges() {
        assert!(!within_secondary_band(14, 20, 5));
        assert!(!within_secondary_band(26, 20, 5));
    }

    #[test]
    fn secondary_band_is_symmetric() {
        // Below and above the target by the same distance behave identically.
        assert_eq!(
            within_secondary_band(10, 20, 5),
            within_secondary_band(30, 20, 5)
        );
    }

    #[test]
    fn secondary_band_does_not_overflow_at_extremes() {
        // The i128 widening means the extreme spread cannot panic on overflow;
        // it is simply (correctly) far outside any sane band.
        assert!(!within_secondary_band(i64::MIN, i64::MAX, 5));
        assert!(!within_secondary_band(i64::MAX, i64::MIN, 5));
        // A close pair near the i64 ceiling with a wide band still evaluates
        // (and, crucially, does not panic computing the difference).
        assert!(within_secondary_band(i64::MAX, i64::MAX - 3, i64::MAX));
    }
}
