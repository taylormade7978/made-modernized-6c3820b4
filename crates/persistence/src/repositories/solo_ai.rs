//! Solo/AI bounded context adapters: [`AIProfileRepository`] and
//! [`MissionAttemptRepository`] over the `0006_solo_ai.sql` tables.

use sqlx::PgPool;

use crate::error::RepositoryError;
use crate::repositories::conflict_or_missing;

/// A row of `ai_profiles`: a tunable AI opponent definition.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AIProfileRow {
    /// Aggregate identity.
    pub id: String,
    /// Profile name.
    pub name: String,
    /// One of `Easy | Normal | Hard | Nightmare` (schema `CHECK`).
    pub difficulty: String,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `AIProfile` aggregate.
#[derive(Debug, Clone)]
pub struct AIProfileRepository {
    pool: PgPool,
}

impl AIProfileRepository {
    const AGGREGATE: &'static str = "AIProfile";
    const TABLE: &'static str = "ai_profiles";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Define a new AI profile (its `params` JSONB defaults to `{}`).
    pub async fn insert(&self, row: &AIProfileRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO ai_profiles (id, name, difficulty, version) VALUES ($1, $2, $3, $4)",
        )
        .bind(&row.id)
        .bind(&row.name)
        .bind(&row.difficulty)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load an AI profile by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<AIProfileRow>, RepositoryError> {
        sqlx::query_as::<_, AIProfileRow>(
            "SELECT id, name, difficulty, version FROM ai_profiles WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Retune a profile's difficulty, guarding on `expected_version`.
    pub async fn update(
        &self,
        row: &AIProfileRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE ai_profiles SET name = $2, difficulty = $3, version = version + 1 \
             WHERE id = $1 AND version = $4",
        )
        .bind(&row.id)
        .bind(&row.name)
        .bind(&row.difficulty)
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

/// A row of `mission_attempts`: one player's run at a solo mission.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct MissionAttemptRow {
    /// Aggregate identity.
    pub id: String,
    /// The attempting player.
    pub player_id: String,
    /// The mission being attempted.
    pub mission_id: String,
    /// The AI opponent, if any (nullable — scripted missions have none).
    pub ai_profile_id: Option<String>,
    /// One of `InProgress | Cleared | Failed | Abandoned`.
    pub status: String,
    /// Score accrued (non-negative).
    pub score: i32,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `MissionAttempt` aggregate.
#[derive(Debug, Clone)]
pub struct MissionAttemptRepository {
    pool: PgPool,
}

impl MissionAttemptRepository {
    const AGGREGATE: &'static str = "MissionAttempt";
    const TABLE: &'static str = "mission_attempts";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Begin a mission attempt.
    pub async fn insert(&self, row: &MissionAttemptRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO mission_attempts \
             (id, player_id, mission_id, ai_profile_id, status, score, version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&row.id)
        .bind(&row.player_id)
        .bind(&row.mission_id)
        .bind(&row.ai_profile_id)
        .bind(&row.status)
        .bind(row.score)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a mission attempt by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<MissionAttemptRow>, RepositoryError> {
        sqlx::query_as::<_, MissionAttemptRow>(
            "SELECT id, player_id, mission_id, ai_profile_id, status, score, version \
             FROM mission_attempts WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Advance a mission attempt's state/score, guarding on `expected_version`.
    pub async fn update(
        &self,
        row: &MissionAttemptRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE mission_attempts SET status = $2, score = $3, version = version + 1 \
             WHERE id = $1 AND version = $4",
        )
        .bind(&row.id)
        .bind(&row.status)
        .bind(row.score)
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

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn ai_profile_round_trips_and_rejects_bad_difficulty(pool: PgPool) {
        let repo = AIProfileRepository::new(pool);
        let profile = AIProfileRow {
            id: "ai-1".to_string(),
            name: "Boss AI".to_string(),
            difficulty: "Hard".to_string(),
            version: 0,
        };
        repo.insert(&profile).await.unwrap();
        assert_eq!(repo.find_by_id("ai-1").await.unwrap().unwrap(), profile);

        let bad = AIProfileRow {
            id: "ai-2".to_string(),
            difficulty: "Impossible".to_string(),
            ..profile
        };
        let err = repo.insert(&bad).await.unwrap_err();
        assert!(
            err.is_invariant_violation(),
            "expected CHECK violation, got {err:?}"
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn mission_attempt_round_trips_without_ai_profile(pool: PgPool) {
        let repo = MissionAttemptRepository::new(pool);
        let attempt = MissionAttemptRow {
            id: "att-1".to_string(),
            player_id: "player-1".to_string(),
            mission_id: "mission-7".to_string(),
            ai_profile_id: None,
            status: "InProgress".to_string(),
            score: 0,
            version: 0,
        };
        repo.insert(&attempt).await.unwrap();
        assert_eq!(repo.find_by_id("att-1").await.unwrap().unwrap(), attempt);
    }
}
