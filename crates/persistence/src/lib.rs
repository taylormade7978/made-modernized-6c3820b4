//! PostgreSQL persistence adapter for MADE.
//!
//! This crate is an *outer adapter* of the hexagon: it owns the physical schema
//! (the `migrations/` at the repo root) and the machinery that applies it, plus
//! the first typed read models over that schema. It deliberately keeps the
//! domain kernel (`shared`) and the bounded contexts (`domain`) framework-free —
//! nothing in those crates depends on `sqlx`.
//!
//! Two capabilities live here today:
//!
//! * [`connect`] / [`run_migrations`] — open a pool and bring a database up to
//!   the latest migration. The embedded [`MIGRATOR`] is the same set of SQL
//!   files that `sqlx migrate run` applies in CI, so local and CI runners can
//!   never drift.
//! * [`leaderboard`] — the hot ranked-standings read path, expressed as a
//!   compile-time-checked query so its shape is validated against the schema at
//!   build time (and cached as offline metadata under `.sqlx/` for CI).
//! * [`repositories`] — the sqlx-backed repository adapters for every durable
//!   aggregate: row mapping, optimistic-concurrency writes that surface a typed
//!   [`RepositoryError::Conflict`], and transactions for the multi-row
//!   invariants (the emission ledger, collection grants, pack opening, an order
//!   and its line items). See that module's docs for why they are async,
//!   owned-value adapters rather than literal `impl shared::Repository`.
//!
//! PostgreSQL is non-substitutable for MADE per the hard platform constraint;
//! there is intentionally no other backend behind these ports.

use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;

/// Re-export of the sqlx connection pool so driving adapters (the actix-web
/// server) can name and hold the pool without taking a direct `sqlx`
/// dependency. The pool is what every repository adapter here is built over.
pub use sqlx::postgres::PgPool;

pub mod error;
pub mod repositories;

pub use error::RepositoryError;

/// The versioned migration set, embedded at compile time from the repo-root
/// `migrations/` directory. [`run_migrations`] applies it; the very same files
/// are what `sqlx migrate run --source migrations` applies in CI, so the two
/// runners stay byte-for-byte identical.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Open a connection pool to the database at `database_url`.
///
/// The pool is sized modestly (up to 5 connections) — enough for the migration
/// runner and the read models here, and a sane default a caller can widen.
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
}

/// Build a pool that connects *lazily* — no TCP connection is opened until the
/// first query runs. Only the `database_url` is validated up front.
///
/// The driving server uses this so it can bind and serve its liveness probe
/// (and mount the `/v1` routes) the instant it starts, even before Postgres is
/// reachable; a connection is then established on demand when the first request
/// actually touches the database. This keeps startup ordering (Kong/OPA
/// sidecars, the DB, the app) from becoming a boot-time dependency.
pub fn connect_lazy(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect_lazy(database_url)
}

/// Apply every pending migration in forward order, bringing the database up to
/// the latest schema. Idempotent: already-applied migrations are skipped via the
/// `_sqlx_migrations` bookkeeping table.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

/// One row of the ranked-standings leaderboard: a player's visible rank
/// (tier + stars) and the hidden rating it is ordered by.
#[derive(Debug, Clone, PartialEq)]
pub struct LeaderboardRow {
    /// The player whose standing this is.
    pub player_id: String,
    /// The hidden Glicko-2 rating the ladder is ordered by (descending).
    pub rating: f64,
    /// The visible tier (Block…Legend).
    pub tier: String,
    /// Stars within the visible tier.
    pub stars: i16,
}

/// The top `limit` standings for `season_id`, highest rating first.
///
/// This is the read path the covering index `idx_ranked_standings_leaderboard`
/// exists for: filtering by `season_id` and ordering by `rating DESC` while
/// projecting only `(player_id, tier, stars)` is served by an *index-only scan*
/// (verified via `EXPLAIN`). The query is compile-time-checked against the
/// schema, so a column rename in a migration is caught at build time.
pub async fn leaderboard(
    pool: &PgPool,
    season_id: &str,
    limit: i64,
) -> Result<Vec<LeaderboardRow>, sqlx::Error> {
    sqlx::query_as!(
        LeaderboardRow,
        r#"
        SELECT player_id, rating, tier, stars
        FROM ranked_standings
        WHERE season_id = $1
        ORDER BY rating DESC
        LIMIT $2
        "#,
        season_id,
        limit
    )
    .fetch_all(pool)
    .await
}
