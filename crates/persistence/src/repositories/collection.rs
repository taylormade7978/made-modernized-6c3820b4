//! Collection-and-deckbuilding bounded context adapters:
//! [`PlayerCollectionRepository`] (the owned-card ledger) and
//! [`OutfitRepository`] over the `0004_collection.sql` tables.
//!
//! Granting cards mutates two tables — the aggregate root's `version` and one
//! ledger row per card — so it runs inside a single transaction. If any card
//! would break the non-negative-balance or copy-cap `CHECK` (e.g. a second
//! Legendary), the whole grant rolls back atomically and the version bump is
//! undone with it.

use sqlx::PgPool;

use crate::error::RepositoryError;
use crate::repositories::{conflict_or_missing, GrantLine};

/// A row of `player_collections`: the aggregate root a ledger hangs off.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PlayerCollectionRow {
    /// Aggregate identity.
    pub id: String,
    /// The owning player (unique — one collection per player).
    pub player_id: String,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// One owned-card balance from `player_collection_cards`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct OwnedCardRow {
    /// The catalog card owned.
    pub card_definition_id: String,
    /// How many copies are held (`0 <= quantity <= max_copies`).
    pub quantity: i32,
    /// The per-card copy cap (the Legendary cap is 1).
    pub max_copies: i32,
}

/// PostgreSQL adapter for the `PlayerCollection` aggregate and its ledger.
#[derive(Debug, Clone)]
pub struct PlayerCollectionRepository {
    pool: PgPool,
}

impl PlayerCollectionRepository {
    const AGGREGATE: &'static str = "PlayerCollection";
    const TABLE: &'static str = "player_collections";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create an empty collection for a player.
    pub async fn insert(&self, row: &PlayerCollectionRow) -> Result<(), RepositoryError> {
        sqlx::query("INSERT INTO player_collections (id, player_id, version) VALUES ($1, $2, $3)")
            .bind(&row.id)
            .bind(&row.player_id)
            .bind(row.version)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load the collection root by identity.
    pub async fn find_by_id(
        &self,
        id: &str,
    ) -> Result<Option<PlayerCollectionRow>, RepositoryError> {
        sqlx::query_as::<_, PlayerCollectionRow>(
            "SELECT id, player_id, version FROM player_collections WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// The owned-card ledger for a collection, ordered by card id.
    pub async fn find_cards(
        &self,
        collection_id: &str,
    ) -> Result<Vec<OwnedCardRow>, RepositoryError> {
        sqlx::query_as::<_, OwnedCardRow>(
            "SELECT card_definition_id, quantity, max_copies \
             FROM player_collection_cards WHERE collection_id = $1 \
             ORDER BY card_definition_id",
        )
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Atomically grant cards to a collection.
    ///
    /// Bumps the aggregate `version` (guarded on `expected_version`) and applies
    /// each [`GrantLine`] as an upsert onto the ledger — all inside one
    /// transaction. A stale version yields a [`RepositoryError::Conflict`]; a
    /// grant that would exceed the copy cap or drive a balance negative trips a
    /// `CHECK` and rolls the entire batch back
    /// ([`RepositoryError::InvariantViolation`]).
    pub async fn grant_cards(
        &self,
        collection_id: &str,
        expected_version: i64,
        grants: &[GrantLine],
    ) -> Result<(), RepositoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        let bumped = sqlx::query(
            "UPDATE player_collections SET version = version + 1 WHERE id = $1 AND version = $2",
        )
        .bind(collection_id)
        .bind(expected_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        if bumped.rows_affected() == 0 {
            let err = conflict_or_missing(
                &mut *tx,
                Self::AGGREGATE,
                Self::TABLE,
                collection_id,
                expected_version,
            )
            .await;
            // Dropping `tx` without commit rolls the (empty) transaction back.
            return Err(err);
        }

        for grant in grants {
            sqlx::query(
                "INSERT INTO player_collection_cards \
                 (collection_id, card_definition_id, quantity, max_copies) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (collection_id, card_definition_id) DO UPDATE \
                 SET quantity = player_collection_cards.quantity + EXCLUDED.quantity, \
                     max_copies = EXCLUDED.max_copies",
            )
            .bind(collection_id)
            .bind(&grant.card_definition_id)
            .bind(grant.quantity)
            .bind(grant.max_copies)
            .execute(&mut *tx)
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        }

        tx.commit()
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }
}

/// A row of `outfits`: a named deck + cosmetic loadout.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct OutfitRow {
    /// Aggregate identity.
    pub id: String,
    /// The owning player.
    pub player_id: String,
    /// The collection this outfit draws its cards from (FK).
    pub collection_id: String,
    /// Outfit name.
    pub name: String,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `Outfit` aggregate.
#[derive(Debug, Clone)]
pub struct OutfitRepository {
    pool: PgPool,
}

impl OutfitRepository {
    const AGGREGATE: &'static str = "Outfit";
    const TABLE: &'static str = "outfits";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new outfit.
    pub async fn insert(&self, row: &OutfitRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO outfits (id, player_id, collection_id, name, version) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&row.id)
        .bind(&row.player_id)
        .bind(&row.collection_id)
        .bind(&row.name)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load an outfit by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<OutfitRow>, RepositoryError> {
        sqlx::query_as::<_, OutfitRow>(
            "SELECT id, player_id, collection_id, name, version FROM outfits WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Rename an outfit, guarding on `expected_version`.
    pub async fn update(
        &self,
        row: &OutfitRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE outfits SET name = $2, version = version + 1 WHERE id = $1 AND version = $3",
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
    use crate::repositories::content::{
        CardDefinitionRepository, CardDefinitionRow, ExpansionSetRepository, ExpansionSetRow,
    };

    async fn seed_card(pool: &PgPool, card_id: &str, rarity: &str) {
        let exp_id = format!("exp-{card_id}");
        ExpansionSetRepository::new(pool.clone())
            .insert(&ExpansionSetRow {
                id: exp_id.clone(),
                code: format!("C-{card_id}"),
                name: "E".to_string(),
                version: 0,
            })
            .await
            .unwrap();
        CardDefinitionRepository::new(pool.clone())
            .insert(&CardDefinitionRow {
                id: card_id.to_string(),
                expansion_set_id: exp_id,
                name: card_id.to_string(),
                rarity: rarity.to_string(),
                cost: 1,
                effect_ref: None,
                version: 0,
            })
            .await
            .unwrap();
    }

    async fn seed_collection(pool: &PgPool, id: &str, player: &str) {
        PlayerCollectionRepository::new(pool.clone())
            .insert(&PlayerCollectionRow {
                id: id.to_string(),
                player_id: player.to_string(),
                version: 0,
            })
            .await
            .unwrap();
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn granting_cards_commits_ledger_and_bumps_version(pool: PgPool) {
        seed_card(&pool, "common-1", "Common").await;
        seed_collection(&pool, "col-1", "player-1").await;
        let repo = PlayerCollectionRepository::new(pool);

        repo.grant_cards("col-1", 0, &[GrantLine::new("common-1", 2, 3)])
            .await
            .unwrap();
        // A second grant off the new version stacks onto the same ledger row.
        repo.grant_cards("col-1", 1, &[GrantLine::new("common-1", 1, 3)])
            .await
            .unwrap();

        let cards = repo.find_cards("col-1").await.unwrap();
        assert_eq!(
            cards,
            vec![OwnedCardRow {
                card_definition_id: "common-1".to_string(),
                quantity: 3,
                max_copies: 3
            }]
        );
        assert_eq!(repo.find_by_id("col-1").await.unwrap().unwrap().version, 2);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn grant_exceeding_copy_cap_rolls_back_atomically(pool: PgPool) {
        seed_card(&pool, "legendary-1", "Legendary").await;
        seed_card(&pool, "common-2", "Common").await;
        seed_collection(&pool, "col-2", "player-2").await;
        let repo = PlayerCollectionRepository::new(pool);

        // A batch where the Legendary (cap 1) is over-granted: the whole
        // transaction must roll back — neither card lands, version stays 0.
        let err = repo
            .grant_cards(
                "col-2",
                0,
                &[
                    GrantLine::new("common-2", 1, 3),
                    GrantLine::new("legendary-1", 2, 1),
                ],
            )
            .await
            .unwrap_err();
        assert!(
            err.is_invariant_violation(),
            "expected CHECK violation, got {err:?}"
        );

        assert!(repo.find_cards("col-2").await.unwrap().is_empty());
        assert_eq!(repo.find_by_id("col-2").await.unwrap().unwrap().version, 0);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn stale_grant_is_a_typed_conflict(pool: PgPool) {
        seed_card(&pool, "common-3", "Common").await;
        seed_collection(&pool, "col-3", "player-3").await;
        let repo = PlayerCollectionRepository::new(pool);

        repo.grant_cards("col-3", 0, &[GrantLine::new("common-3", 1, 3)])
            .await
            .unwrap();
        // Version is now 1; a writer still on 0 must be rejected.
        let err = repo
            .grant_cards("col-3", 0, &[GrantLine::new("common-3", 1, 3)])
            .await
            .unwrap_err();
        assert!(err.is_conflict(), "expected Conflict, got {err:?}");
        // The rejected grant did not touch the ledger.
        assert_eq!(
            repo.find_cards("col-3").await.unwrap()[0].quantity,
            1,
            "rejected grant must not have stacked"
        );
    }
}
