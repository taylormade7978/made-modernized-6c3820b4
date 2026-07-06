//! Token-and-marketplace bounded context adapters: [`CardTokenRepository`],
//! [`MarketplaceListingRepository`], and [`EmissionPoolRepository`] over the
//! `0007_token_marketplace.sql` tables.
//!
//! Emitting a reward debits the pool balance *and* appends an audit row to the
//! `emission_ledger`, so it runs in a single transaction: the debit is guarded
//! on the pool's version, the schema's solvency `CHECK` forbids overdraw, and
//! the ledger append only commits if the debit did.

use sqlx::PgPool;

use crate::error::RepositoryError;
use crate::repositories::conflict_or_missing;

/// A row of `card_tokens`: a single mintable ERC-1155 card token.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CardTokenRow {
    /// Aggregate identity.
    pub id: String,
    /// On-chain ERC-1155 token id.
    pub token_id: String,
    /// Unique, non-reusable serial for serialized editions (nullable).
    pub serial_number: Option<String>,
    /// Staged IPFS metadata record (nullable).
    pub metadata_uri: Option<String>,
    /// Linked custodial / WalletConnect wallet (nullable).
    pub owner_wallet: Option<String>,
    /// Whether the token has been minted on chain.
    pub minted: bool,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `CardToken` aggregate.
#[derive(Debug, Clone)]
pub struct CardTokenRepository {
    pool: PgPool,
}

impl CardTokenRepository {
    const AGGREGATE: &'static str = "CardToken";
    const TABLE: &'static str = "card_tokens";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Stage a new token. A reused `serial_number` trips the `UNIQUE` constraint
    /// and surfaces as an invariant violation.
    pub async fn insert(&self, row: &CardTokenRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO card_tokens \
             (id, token_id, serial_number, metadata_uri, owner_wallet, minted, version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&row.id)
        .bind(&row.token_id)
        .bind(&row.serial_number)
        .bind(&row.metadata_uri)
        .bind(&row.owner_wallet)
        .bind(row.minted)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a token by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<CardTokenRow>, RepositoryError> {
        sqlx::query_as::<_, CardTokenRow>(
            "SELECT id, token_id, serial_number, metadata_uri, owner_wallet, minted, version \
             FROM card_tokens WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Mint / relink a token, guarding on `expected_version`.
    pub async fn update(
        &self,
        row: &CardTokenRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE card_tokens \
             SET metadata_uri = $2, owner_wallet = $3, minted = $4, version = version + 1 \
             WHERE id = $1 AND version = $5",
        )
        .bind(&row.id)
        .bind(&row.metadata_uri)
        .bind(&row.owner_wallet)
        .bind(row.minted)
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

/// A row of `marketplace_listings`: an owned token listed for sale in $MADE.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct MarketplaceListingRow {
    /// Aggregate identity.
    pub id: String,
    /// The token on offer (FK).
    pub card_token_id: String,
    /// The seller.
    pub seller_id: String,
    /// Ask price in $MADE (strictly positive).
    pub price_made: i64,
    /// One of `Open | Cancelled | Purchased | Settled`.
    pub status: String,
    /// Jurisdiction gate (nullable).
    pub jurisdiction: Option<String>,
    /// The buyer, once matched (nullable).
    pub buyer_id: Option<String>,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `MarketplaceListing` aggregate.
#[derive(Debug, Clone)]
pub struct MarketplaceListingRepository {
    pool: PgPool,
}

impl MarketplaceListingRepository {
    const AGGREGATE: &'static str = "MarketplaceListing";
    const TABLE: &'static str = "marketplace_listings";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Open a listing for a token.
    pub async fn insert(&self, row: &MarketplaceListingRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO marketplace_listings \
             (id, card_token_id, seller_id, price_made, status, jurisdiction, buyer_id, version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&row.id)
        .bind(&row.card_token_id)
        .bind(&row.seller_id)
        .bind(row.price_made)
        .bind(&row.status)
        .bind(&row.jurisdiction)
        .bind(&row.buyer_id)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a listing by identity.
    pub async fn find_by_id(
        &self,
        id: &str,
    ) -> Result<Option<MarketplaceListingRow>, RepositoryError> {
        sqlx::query_as::<_, MarketplaceListingRow>(
            "SELECT id, card_token_id, seller_id, price_made, status, jurisdiction, buyer_id, version \
             FROM marketplace_listings WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Transition a listing (cancel/purchase/settle), guarding on
    /// `expected_version`.
    pub async fn update(
        &self,
        row: &MarketplaceListingRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE marketplace_listings SET status = $2, buyer_id = $3, version = version + 1 \
             WHERE id = $1 AND version = $4",
        )
        .bind(&row.id)
        .bind(&row.status)
        .bind(&row.buyer_id)
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

/// A row of `emission_pools`: the per-season $MADE reward pool.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct EmissionPoolRow {
    /// Aggregate identity.
    pub id: String,
    /// The season this pool funds (unique FK — one pool per season).
    pub season_id: String,
    /// The minted starting balance (the solvency ceiling).
    pub starting_balance: i64,
    /// The remaining balance (`0 <= remaining <= starting`).
    pub remaining_balance: i64,
    /// Whether a low-pool warning has been raised.
    pub low_pool_warned: bool,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `EmissionPool` aggregate and its emission ledger.
#[derive(Debug, Clone)]
pub struct EmissionPoolRepository {
    pool: PgPool,
}

impl EmissionPoolRepository {
    const AGGREGATE: &'static str = "EmissionPool";
    const TABLE: &'static str = "emission_pools";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Open a pool with its minted starting balance.
    pub async fn insert(&self, row: &EmissionPoolRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO emission_pools \
             (id, season_id, starting_balance, remaining_balance, low_pool_warned, version) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&row.id)
        .bind(&row.season_id)
        .bind(row.starting_balance)
        .bind(row.remaining_balance)
        .bind(row.low_pool_warned)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a pool by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<EmissionPoolRow>, RepositoryError> {
        sqlx::query_as::<_, EmissionPoolRow>(
            "SELECT id, season_id, starting_balance, remaining_balance, low_pool_warned, version \
             FROM emission_pools WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// The number of emission ledger rows drawn from a pool (audit-trail size).
    pub async fn ledger_len(&self, pool_id: &str) -> Result<i64, RepositoryError> {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM emission_ledger WHERE pool_id = $1")
            .bind(pool_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Emit a reward from a pool: atomically debit `amount` from the remaining
    /// balance (guarded on `expected_version`) and append the matching
    /// `emission_ledger` row.
    ///
    /// A stale version yields a [`RepositoryError::Conflict`]; an overdraw trips
    /// the solvency `CHECK` (`remaining_balance >= 0`) and rolls the whole
    /// emission — debit *and* ledger append — back atomically.
    pub async fn emit_reward(
        &self,
        pool_id: &str,
        expected_version: i64,
        ledger_id: &str,
        recipient_id: &str,
        amount: i64,
    ) -> Result<(), RepositoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        let debited = sqlx::query(
            "UPDATE emission_pools \
             SET remaining_balance = remaining_balance - $3, version = version + 1 \
             WHERE id = $1 AND version = $2",
        )
        .bind(pool_id)
        .bind(expected_version)
        .bind(amount)
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        if debited.rows_affected() == 0 {
            let err = conflict_or_missing(
                &mut *tx,
                Self::AGGREGATE,
                Self::TABLE,
                pool_id,
                expected_version,
            )
            .await;
            return Err(err);
        }

        sqlx::query(
            "INSERT INTO emission_ledger (id, pool_id, recipient_id, amount) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(ledger_id)
        .bind(pool_id)
        .bind(recipient_id)
        .bind(amount)
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        tx.commit()
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }
}

#[cfg(all(test, feature = "integration-tests"))]
mod tests {
    use super::*;
    use crate::repositories::matchmaking::{SeasonRepository, SeasonRow};

    async fn seed_pool(pool: &PgPool, pool_id: &str, season_num: i32, starting: i64) {
        let season_id = format!("season-{pool_id}");
        SeasonRepository::new(pool.clone())
            .insert(&SeasonRow {
                id: season_id.clone(),
                number: season_num,
                name: "S".to_string(),
                version: 0,
            })
            .await
            .unwrap();
        EmissionPoolRepository::new(pool.clone())
            .insert(&EmissionPoolRow {
                id: pool_id.to_string(),
                season_id,
                starting_balance: starting,
                remaining_balance: starting,
                low_pool_warned: false,
                version: 0,
            })
            .await
            .unwrap();
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn card_token_serials_are_unique(pool: PgPool) {
        let repo = CardTokenRepository::new(pool);
        let token = CardTokenRow {
            id: "tok-1".to_string(),
            token_id: "chain-1".to_string(),
            serial_number: Some("SN-0001".to_string()),
            metadata_uri: None,
            owner_wallet: None,
            minted: false,
            version: 0,
        };
        repo.insert(&token).await.unwrap();
        assert_eq!(repo.find_by_id("tok-1").await.unwrap().unwrap(), token);

        // A second token reusing the serial is rejected.
        let dup = CardTokenRow {
            id: "tok-2".to_string(),
            token_id: "chain-2".to_string(),
            ..token
        };
        let err = repo.insert(&dup).await.unwrap_err();
        assert!(
            err.is_invariant_violation(),
            "expected UNIQUE violation, got {err:?}"
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn emitting_a_reward_debits_and_appends_atomically(pool: PgPool) {
        seed_pool(&pool, "pool-1", 1, 1000).await;
        let repo = EmissionPoolRepository::new(pool);

        repo.emit_reward("pool-1", 0, "led-1", "winner", 400)
            .await
            .unwrap();
        let after = repo.find_by_id("pool-1").await.unwrap().unwrap();
        assert_eq!(after.remaining_balance, 600);
        assert_eq!(after.version, 1);
        assert_eq!(repo.ledger_len("pool-1").await.unwrap(), 1);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn overdrawing_a_pool_rolls_back_debit_and_ledger(pool: PgPool) {
        seed_pool(&pool, "pool-2", 2, 100).await;
        let repo = EmissionPoolRepository::new(pool);

        // Draw more than the pool holds: the solvency CHECK must roll the whole
        // emission back — balance untouched, no ledger row.
        let err = repo
            .emit_reward("pool-2", 0, "led-2", "winner", 500)
            .await
            .unwrap_err();
        assert!(
            err.is_invariant_violation(),
            "expected CHECK violation, got {err:?}"
        );

        let after = repo.find_by_id("pool-2").await.unwrap().unwrap();
        assert_eq!(after.remaining_balance, 100);
        assert_eq!(after.version, 0);
        assert_eq!(repo.ledger_len("pool-2").await.unwrap(), 0);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn stale_emit_is_a_typed_conflict(pool: PgPool) {
        seed_pool(&pool, "pool-3", 3, 1000).await;
        let repo = EmissionPoolRepository::new(pool);

        repo.emit_reward("pool-3", 0, "led-3", "winner", 100)
            .await
            .unwrap();
        // Version is now 1; a stale writer on 0 is rejected and appends nothing.
        let err = repo
            .emit_reward("pool-3", 0, "led-4", "winner", 100)
            .await
            .unwrap_err();
        assert!(err.is_conflict(), "expected Conflict, got {err:?}");
        assert_eq!(repo.ledger_len("pool-3").await.unwrap(), 1);
    }
}
