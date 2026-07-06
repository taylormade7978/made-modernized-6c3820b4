//! Content bounded context adapters: the card catalog every other context
//! references — [`ExpansionSetRepository`], [`CardDefinitionRepository`], and
//! [`BossDefinitionRepository`] over the `0001_content.sql` tables.

use sqlx::PgPool;

use crate::error::RepositoryError;
use crate::repositories::conflict_or_missing;

/// A row of `expansion_sets`: a released wave of content.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ExpansionSetRow {
    /// Aggregate identity.
    pub id: String,
    /// Stable short code, e.g. `"BASE"` (unique).
    pub code: String,
    /// Human-readable name.
    pub name: String,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `ExpansionSet` aggregate.
#[derive(Debug, Clone)]
pub struct ExpansionSetRepository {
    pool: PgPool,
}

impl ExpansionSetRepository {
    const AGGREGATE: &'static str = "ExpansionSet";
    const TABLE: &'static str = "expansion_sets";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert a new expansion set.
    pub async fn insert(&self, row: &ExpansionSetRow) -> Result<(), RepositoryError> {
        sqlx::query("INSERT INTO expansion_sets (id, code, name, version) VALUES ($1, $2, $3, $4)")
            .bind(&row.id)
            .bind(&row.code)
            .bind(&row.name)
            .bind(row.version)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load an expansion set by identity, reconstructing it identically.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<ExpansionSetRow>, RepositoryError> {
        sqlx::query_as::<_, ExpansionSetRow>(
            "SELECT id, code, name, version FROM expansion_sets WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Persist a mutation, guarding on `expected_version`. Returns
    /// [`RepositoryError::Conflict`] if the persisted version has moved on.
    pub async fn update(
        &self,
        row: &ExpansionSetRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE expansion_sets SET code = $2, name = $3, version = version + 1 \
             WHERE id = $1 AND version = $4",
        )
        .bind(&row.id)
        .bind(&row.code)
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

/// A row of `card_definitions`: one card in the catalog.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CardDefinitionRow {
    /// Aggregate identity.
    pub id: String,
    /// The expansion this card belongs to (FK).
    pub expansion_set_id: String,
    /// Card name.
    pub name: String,
    /// One of `Common | Rare | Epic | Legendary` (schema `CHECK`).
    pub rarity: String,
    /// Mana/energy cost (non-negative).
    pub cost: i32,
    /// Optional effect script reference.
    pub effect_ref: Option<String>,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `CardDefinition` aggregate.
#[derive(Debug, Clone)]
pub struct CardDefinitionRepository {
    pool: PgPool,
}

impl CardDefinitionRepository {
    const AGGREGATE: &'static str = "CardDefinition";
    const TABLE: &'static str = "card_definitions";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Catalog a new card definition.
    pub async fn insert(&self, row: &CardDefinitionRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO card_definitions \
             (id, expansion_set_id, name, rarity, cost, effect_ref, version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&row.id)
        .bind(&row.expansion_set_id)
        .bind(&row.name)
        .bind(&row.rarity)
        .bind(row.cost)
        .bind(&row.effect_ref)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a card definition by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<CardDefinitionRow>, RepositoryError> {
        sqlx::query_as::<_, CardDefinitionRow>(
            "SELECT id, expansion_set_id, name, rarity, cost, effect_ref, version \
             FROM card_definitions WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Amend a card (balance/text), guarding on `expected_version`.
    pub async fn update(
        &self,
        row: &CardDefinitionRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE card_definitions \
             SET name = $2, rarity = $3, cost = $4, effect_ref = $5, version = version + 1 \
             WHERE id = $1 AND version = $6",
        )
        .bind(&row.id)
        .bind(&row.name)
        .bind(&row.rarity)
        .bind(row.cost)
        .bind(&row.effect_ref)
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

/// A row of `boss_definitions`: a scripted PvE encounter.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct BossDefinitionRow {
    /// Aggregate identity.
    pub id: String,
    /// The expansion this boss ships inside (FK).
    pub expansion_set_id: String,
    /// Boss name.
    pub name: String,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `BossDefinition` aggregate.
#[derive(Debug, Clone)]
pub struct BossDefinitionRepository {
    pool: PgPool,
}

impl BossDefinitionRepository {
    const AGGREGATE: &'static str = "BossDefinition";
    const TABLE: &'static str = "boss_definitions";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Define a new boss (its `roster` JSONB defaults to `{}`).
    pub async fn insert(&self, row: &BossDefinitionRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO boss_definitions (id, expansion_set_id, name, version) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&row.id)
        .bind(&row.expansion_set_id)
        .bind(&row.name)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a boss definition by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<BossDefinitionRow>, RepositoryError> {
        sqlx::query_as::<_, BossDefinitionRow>(
            "SELECT id, expansion_set_id, name, version FROM boss_definitions WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Rename/retarget a boss, guarding on `expected_version`.
    pub async fn update(
        &self,
        row: &BossDefinitionRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE boss_definitions SET name = $2, version = version + 1 \
             WHERE id = $1 AND version = $3",
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

#[cfg(all(test, feature = "integration-tests"))]
mod tests {
    use super::*;

    fn expansion(id: &str) -> ExpansionSetRow {
        ExpansionSetRow {
            id: id.to_string(),
            code: format!("code-{id}"),
            name: format!("Expansion {id}"),
            version: 0,
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn expansion_set_round_trips(pool: PgPool) {
        let repo = ExpansionSetRepository::new(pool);
        let row = expansion("exp-1");
        repo.insert(&row).await.unwrap();

        let loaded = repo.find_by_id("exp-1").await.unwrap().unwrap();
        assert_eq!(loaded, row);
        assert!(repo.find_by_id("nope").await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn stale_expansion_update_is_a_typed_conflict(pool: PgPool) {
        let repo = ExpansionSetRepository::new(pool);
        let mut row = expansion("exp-2");
        repo.insert(&row).await.unwrap();

        // First update off version 0 succeeds and advances the row to version 1.
        row.name = "Renamed".to_string();
        repo.update(&row, 0).await.unwrap();
        assert_eq!(repo.find_by_id("exp-2").await.unwrap().unwrap().version, 1);

        // A second writer still holding version 0 loses the race — typed conflict,
        // no silent overwrite.
        let err = repo.update(&row, 0).await.unwrap_err();
        assert!(err.is_conflict(), "expected Conflict, got {err:?}");

        // Updating a row that never existed is NotFound, not Conflict.
        let missing = repo.update(&expansion("ghost"), 0).await.unwrap_err();
        assert!(matches!(missing, RepositoryError::NotFound { .. }));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn card_definition_round_trips_under_its_expansion(pool: PgPool) {
        ExpansionSetRepository::new(pool.clone())
            .insert(&expansion("exp-3"))
            .await
            .unwrap();
        let repo = CardDefinitionRepository::new(pool);
        let card = CardDefinitionRow {
            id: "card-1".to_string(),
            expansion_set_id: "exp-3".to_string(),
            name: "Fireball".to_string(),
            rarity: "Rare".to_string(),
            cost: 4,
            effect_ref: Some("effects/fireball".to_string()),
            version: 0,
        };
        repo.insert(&card).await.unwrap();
        assert_eq!(repo.find_by_id("card-1").await.unwrap().unwrap(), card);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn card_definition_rejects_bad_rarity(pool: PgPool) {
        ExpansionSetRepository::new(pool.clone())
            .insert(&expansion("exp-4"))
            .await
            .unwrap();
        let repo = CardDefinitionRepository::new(pool);
        let bad = CardDefinitionRow {
            id: "card-2".to_string(),
            expansion_set_id: "exp-4".to_string(),
            name: "Broken".to_string(),
            rarity: "Mythic".to_string(), // not in the CHECK set
            cost: 1,
            effect_ref: None,
            version: 0,
        };
        let err = repo.insert(&bad).await.unwrap_err();
        assert!(
            err.is_invariant_violation(),
            "expected CHECK violation, got {err:?}"
        );
    }
}
