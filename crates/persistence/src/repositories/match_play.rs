//! Match-play bounded context adapters: live authoritative matches
//! ([`GameSessionRepository`]) and their sealed replays
//! ([`MatchReplayRepository`]) over the `0002_match_play.sql` tables.

use sqlx::PgPool;

use crate::error::RepositoryError;
use crate::repositories::conflict_or_missing;

/// A row of `game_sessions`: one live authoritative match.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct GameSessionRow {
    /// Aggregate identity.
    pub id: String,
    /// The player hosting the match.
    pub host_player_id: String,
    /// One of `Pending | Active | Conceded | Completed | Abandoned`.
    pub status: String,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `GameSession` aggregate.
#[derive(Debug, Clone)]
pub struct GameSessionRepository {
    pool: PgPool,
}

impl GameSessionRepository {
    const AGGREGATE: &'static str = "GameSession";
    const TABLE: &'static str = "game_sessions";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Open a new session row.
    pub async fn insert(&self, row: &GameSessionRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO game_sessions (id, host_player_id, status, version) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&row.id)
        .bind(&row.host_player_id)
        .bind(&row.status)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a session by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<GameSessionRow>, RepositoryError> {
        sqlx::query_as::<_, GameSessionRow>(
            "SELECT id, host_player_id, status, version FROM game_sessions WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Advance the session's lifecycle status, guarding on `expected_version`.
    pub async fn update(
        &self,
        row: &GameSessionRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE game_sessions SET status = $2, version = version + 1 \
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

/// A row of `match_replays`: the sealed, immutable record of a finished session.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct MatchReplayRow {
    /// Aggregate identity.
    pub id: String,
    /// The session this replay records (unique FK — one replay per session).
    pub session_id: String,
    /// Whether the replay has been sealed.
    pub sealed: bool,
    /// Frame-stream checksum; the schema requires it once `sealed` is true.
    pub checksum: Option<String>,
    /// URI of the sealed frame stream.
    pub frame_uri: Option<String>,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `MatchReplay` aggregate.
#[derive(Debug, Clone)]
pub struct MatchReplayRepository {
    pool: PgPool,
}

impl MatchReplayRepository {
    const AGGREGATE: &'static str = "MatchReplay";
    const TABLE: &'static str = "match_replays";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Open a replay record for a session.
    pub async fn insert(&self, row: &MatchReplayRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO match_replays (id, session_id, sealed, checksum, frame_uri, version) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&row.id)
        .bind(&row.session_id)
        .bind(row.sealed)
        .bind(&row.checksum)
        .bind(&row.frame_uri)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a replay by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<MatchReplayRow>, RepositoryError> {
        sqlx::query_as::<_, MatchReplayRow>(
            "SELECT id, session_id, sealed, checksum, frame_uri, version \
             FROM match_replays WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Seal (or amend) a replay, guarding on `expected_version`. A sealed replay
    /// without a checksum is rejected by the schema and surfaces as an
    /// [`RepositoryError::InvariantViolation`].
    pub async fn update(
        &self,
        row: &MatchReplayRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE match_replays \
             SET sealed = $2, checksum = $3, frame_uri = $4, version = version + 1 \
             WHERE id = $1 AND version = $5",
        )
        .bind(&row.id)
        .bind(row.sealed)
        .bind(&row.checksum)
        .bind(&row.frame_uri)
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

    fn session(id: &str) -> GameSessionRow {
        GameSessionRow {
            id: id.to_string(),
            host_player_id: "player-1".to_string(),
            status: "Pending".to_string(),
            version: 0,
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn game_session_round_trips_and_advances_status(pool: PgPool) {
        let repo = GameSessionRepository::new(pool);
        let mut row = session("s-1");
        repo.insert(&row).await.unwrap();
        assert_eq!(repo.find_by_id("s-1").await.unwrap().unwrap(), row);

        row.status = "Active".to_string();
        repo.update(&row, 0).await.unwrap();
        let loaded = repo.find_by_id("s-1").await.unwrap().unwrap();
        assert_eq!(loaded.status, "Active");
        assert_eq!(loaded.version, 1);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn match_replay_round_trips_and_rejects_sealed_without_checksum(pool: PgPool) {
        GameSessionRepository::new(pool.clone())
            .insert(&session("s-2"))
            .await
            .unwrap();
        let repo = MatchReplayRepository::new(pool);

        let replay = MatchReplayRow {
            id: "r-1".to_string(),
            session_id: "s-2".to_string(),
            sealed: false,
            checksum: None,
            frame_uri: None,
            version: 0,
        };
        repo.insert(&replay).await.unwrap();
        assert_eq!(repo.find_by_id("r-1").await.unwrap().unwrap(), replay);

        // Sealing without a checksum trips match_replays_sealed_has_checksum.
        let bad_seal = MatchReplayRow {
            sealed: true,
            checksum: None,
            ..replay.clone()
        };
        let err = repo.update(&bad_seal, 0).await.unwrap_err();
        assert!(
            err.is_invariant_violation(),
            "expected CHECK violation, got {err:?}"
        );

        // Sealing *with* a checksum succeeds.
        let good_seal = MatchReplayRow {
            sealed: true,
            checksum: Some("deadbeef".to_string()),
            ..replay
        };
        repo.update(&good_seal, 0).await.unwrap();
        assert!(repo.find_by_id("r-1").await.unwrap().unwrap().sealed);
    }
}
