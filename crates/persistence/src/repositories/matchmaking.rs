//! Matchmaking-and-ranked bounded context adapters: [`SeasonRepository`],
//! [`MatchmakingTicketRepository`], and [`RankedStandingRepository`] over the
//! `0003_matchmaking_ranked.sql` tables.

use sqlx::PgPool;

use crate::error::RepositoryError;
use crate::repositories::conflict_or_missing;

/// A row of `seasons`: a competitive season window.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct SeasonRow {
    /// Aggregate identity.
    pub id: String,
    /// Sequential season number (unique, `>= 1`).
    pub number: i32,
    /// Human-readable name.
    pub name: String,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `Season` aggregate.
#[derive(Debug, Clone)]
pub struct SeasonRepository {
    pool: PgPool,
}

impl SeasonRepository {
    const AGGREGATE: &'static str = "Season";
    const TABLE: &'static str = "seasons";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Open a new season.
    pub async fn insert(&self, row: &SeasonRow) -> Result<(), RepositoryError> {
        sqlx::query("INSERT INTO seasons (id, number, name, version) VALUES ($1, $2, $3, $4)")
            .bind(&row.id)
            .bind(row.number)
            .bind(&row.name)
            .bind(row.version)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a season by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<SeasonRow>, RepositoryError> {
        sqlx::query_as::<_, SeasonRow>(
            "SELECT id, number, name, version FROM seasons WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Rename a season, guarding on `expected_version`.
    pub async fn update(
        &self,
        row: &SeasonRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE seasons SET name = $2, version = version + 1 WHERE id = $1 AND version = $3",
        )
        .bind(&row.id)
        .bind(&row.name)
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        if result.rows_affected() == 0 {
            return Err(conflict_or_missing(
                &self.pool,
                Self::AGGREGATE,
                Self::TABLE,
                &row.id,
                expected_version,
            )
            .await);
        }
        Ok(())
    }
}

/// A row of `matchmaking_tickets`: a player's queue entry within a season.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct MatchmakingTicketRow {
    /// Aggregate identity.
    pub id: String,
    /// The queueing player.
    pub player_id: String,
    /// The season this ticket is scoped to (FK).
    pub season_id: String,
    /// One of `Queued | Matched | Cancelled | Expired`.
    pub status: String,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `MatchmakingTicket` aggregate.
#[derive(Debug, Clone)]
pub struct MatchmakingTicketRepository {
    pool: PgPool,
}

impl MatchmakingTicketRepository {
    const AGGREGATE: &'static str = "MatchmakingTicket";
    const TABLE: &'static str = "matchmaking_tickets";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Enqueue a ticket. A second live ticket for the same `(season, player)`
    /// trips the `UNIQUE` constraint and surfaces as an invariant violation.
    pub async fn insert(&self, row: &MatchmakingTicketRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO matchmaking_tickets (id, player_id, season_id, status, version) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&row.id)
        .bind(&row.player_id)
        .bind(&row.season_id)
        .bind(&row.status)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a ticket by identity.
    pub async fn find_by_id(
        &self,
        id: &str,
    ) -> Result<Option<MatchmakingTicketRow>, RepositoryError> {
        sqlx::query_as::<_, MatchmakingTicketRow>(
            "SELECT id, player_id, season_id, status, version \
             FROM matchmaking_tickets WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Transition a ticket's status, guarding on `expected_version`.
    pub async fn update(
        &self,
        row: &MatchmakingTicketRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE matchmaking_tickets SET status = $2, version = version + 1 \
             WHERE id = $1 AND version = $3",
        )
        .bind(&row.id)
        .bind(&row.status)
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        if result.rows_affected() == 0 {
            return Err(conflict_or_missing(
                &self.pool,
                Self::AGGREGATE,
                Self::TABLE,
                &row.id,
                expected_version,
            )
            .await);
        }
        Ok(())
    }
}

/// A row of `ranked_standings`: one player's competitive record for a season —
/// the hidden Glicko-2 estimate plus the visible ladder rank.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RankedStandingRow {
    /// Aggregate identity.
    pub id: String,
    /// The player this standing belongs to.
    pub player_id: String,
    /// The season this standing is scoped to (FK).
    pub season_id: String,
    /// Hidden Glicko-2 rating the ladder is ordered by.
    pub rating: f64,
    /// Glicko-2 rating deviation.
    pub rating_dev: f64,
    /// Glicko-2 volatility.
    pub volatility: f64,
    /// Visible tier (`Block | Corner | Contender | Champion | Legend`).
    pub tier: String,
    /// Stars within the visible tier.
    pub stars: i16,
    /// Anti-tilt floor tier the player cannot fall below.
    pub floor_tier: String,
    /// Ranked matches played this season.
    pub matches_played: i32,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `RankedStanding` aggregate.
#[derive(Debug, Clone)]
pub struct RankedStandingRepository {
    pool: PgPool,
}

impl RankedStandingRepository {
    const AGGREGATE: &'static str = "RankedStanding";
    const TABLE: &'static str = "ranked_standings";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a player's standing for a season.
    pub async fn insert(&self, row: &RankedStandingRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO ranked_standings \
             (id, player_id, season_id, rating, rating_dev, volatility, tier, stars, \
              floor_tier, matches_played, version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&row.id)
        .bind(&row.player_id)
        .bind(&row.season_id)
        .bind(row.rating)
        .bind(row.rating_dev)
        .bind(row.volatility)
        .bind(&row.tier)
        .bind(row.stars)
        .bind(&row.floor_tier)
        .bind(row.matches_played)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a standing by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<RankedStandingRow>, RepositoryError> {
        sqlx::query_as::<_, RankedStandingRow>(
            "SELECT id, player_id, season_id, rating, rating_dev, volatility, tier, stars, \
             floor_tier, matches_played, version \
             FROM ranked_standings WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Persist a recalculated standing (rating + visible rank), guarding on
    /// `expected_version`.
    pub async fn update(
        &self,
        row: &RankedStandingRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE ranked_standings \
             SET rating = $2, rating_dev = $3, volatility = $4, tier = $5, stars = $6, \
                 floor_tier = $7, matches_played = $8, version = version + 1 \
             WHERE id = $1 AND version = $9",
        )
        .bind(&row.id)
        .bind(row.rating)
        .bind(row.rating_dev)
        .bind(row.volatility)
        .bind(&row.tier)
        .bind(row.stars)
        .bind(&row.floor_tier)
        .bind(row.matches_played)
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        if result.rows_affected() == 0 {
            return Err(conflict_or_missing(
                &self.pool,
                Self::AGGREGATE,
                Self::TABLE,
                &row.id,
                expected_version,
            )
            .await);
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "integration-tests"))]
mod tests {
    use super::*;

    fn season(id: &str, number: i32) -> SeasonRow {
        SeasonRow {
            id: id.to_string(),
            number,
            name: format!("Season {number}"),
            version: 0,
        }
    }

    fn standing(id: &str, season_id: &str, player: &str) -> RankedStandingRow {
        RankedStandingRow {
            id: id.to_string(),
            player_id: player.to_string(),
            season_id: season_id.to_string(),
            rating: 1500.0,
            rating_dev: 350.0,
            volatility: 0.06,
            tier: "Block".to_string(),
            stars: 0,
            floor_tier: "Block".to_string(),
            matches_played: 0,
            version: 0,
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn season_and_ticket_round_trip(pool: PgPool) {
        SeasonRepository::new(pool.clone())
            .insert(&season("season-1", 1))
            .await
            .unwrap();

        let tickets = MatchmakingTicketRepository::new(pool);
        let ticket = MatchmakingTicketRow {
            id: "ticket-1".to_string(),
            player_id: "player-1".to_string(),
            season_id: "season-1".to_string(),
            status: "Queued".to_string(),
            version: 0,
        };
        tickets.insert(&ticket).await.unwrap();
        assert_eq!(
            tickets.find_by_id("ticket-1").await.unwrap().unwrap(),
            ticket
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn ranked_standing_round_trips_and_conflicts(pool: PgPool) {
        SeasonRepository::new(pool.clone())
            .insert(&season("season-2", 2))
            .await
            .unwrap();
        let repo = RankedStandingRepository::new(pool);
        let mut row = standing("standing-1", "season-2", "player-9");
        repo.insert(&row).await.unwrap();
        assert_eq!(repo.find_by_id("standing-1").await.unwrap().unwrap(), row);

        // Apply a rating recalculation off version 0.
        row.rating = 1580.5;
        row.stars = 2;
        row.matches_played = 1;
        repo.update(&row, 0).await.unwrap();
        let loaded = repo.find_by_id("standing-1").await.unwrap().unwrap();
        assert_eq!(loaded.rating, 1580.5);
        assert_eq!(loaded.version, 1);

        // A stale writer is rejected without overwriting the fresh rating.
        let err = repo.update(&row, 0).await.unwrap_err();
        assert!(err.is_conflict(), "expected Conflict, got {err:?}");
    }
}
