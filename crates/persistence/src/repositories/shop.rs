//! Shop-and-payments bounded context adapters: [`OrderRepository`] (with its
//! line-item ledger), [`CardPackRepository`] (including the transactional
//! pack-opening grant), and [`BattlePassRepository`] over the
//! `0005_shop_payments.sql` tables.

use sqlx::PgPool;

use crate::error::RepositoryError;
use crate::repositories::{conflict_or_missing, GrantLine};

/// A row of `orders`: one storefront purchase.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct OrderRow {
    /// Aggregate identity.
    pub id: String,
    /// The purchasing player.
    pub player_id: String,
    /// ISO-4217 currency code (exactly 3 chars).
    pub currency: String,
    /// Order total in minor units (cents), non-negative.
    pub total_amount: i64,
    /// One of `Created | PaymentConfirmed | Fulfilled | Refunded`.
    pub status: String,
    /// Stripe payment-intent id (unique idempotency key; `None` until attached).
    pub stripe_payment_intent_id: Option<String>,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// A row of `order_line_items`: one priced line of an order.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct OrderLineItemRow {
    /// Line identity.
    pub id: String,
    /// The owning order (FK).
    pub order_id: String,
    /// Stock-keeping unit purchased.
    pub sku: String,
    /// Per-unit price in minor units (non-negative).
    pub unit_amount: i64,
    /// Quantity purchased (strictly positive).
    pub quantity: i32,
    /// `unit_amount * quantity`, application-computed (non-negative).
    pub line_amount: i64,
}

/// PostgreSQL adapter for the `Order` aggregate and its line-item ledger.
#[derive(Debug, Clone)]
pub struct OrderRepository {
    pool: PgPool,
}

impl OrderRepository {
    const AGGREGATE: &'static str = "Order";
    const TABLE: &'static str = "orders";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Atomically create an order together with all of its line items.
    ///
    /// The order header and every line insert run in one transaction, so a
    /// half-written order (a header with no lines, or lines whose `quantity`
    /// trips the positivity `CHECK`) can never be observed — the batch either
    /// commits whole or rolls back whole.
    pub async fn create_order(
        &self,
        order: &OrderRow,
        items: &[OrderLineItemRow],
    ) -> Result<(), RepositoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        sqlx::query(
            "INSERT INTO orders \
             (id, player_id, currency, total_amount, status, stripe_payment_intent_id, version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&order.id)
        .bind(&order.player_id)
        .bind(&order.currency)
        .bind(order.total_amount)
        .bind(&order.status)
        .bind(&order.stripe_payment_intent_id)
        .bind(order.version)
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        for item in items {
            sqlx::query(
                "INSERT INTO order_line_items \
                 (id, order_id, sku, unit_amount, quantity, line_amount) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(&item.id)
            .bind(&item.order_id)
            .bind(&item.sku)
            .bind(item.unit_amount)
            .bind(item.quantity)
            .bind(item.line_amount)
            .execute(&mut *tx)
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        }

        tx.commit()
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Load an order header by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<OrderRow>, RepositoryError> {
        sqlx::query_as::<_, OrderRow>(
            "SELECT id, player_id, currency, total_amount, status, stripe_payment_intent_id, version \
             FROM orders WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// The line items of an order, in insertion (id) order.
    pub async fn find_line_items(
        &self,
        order_id: &str,
    ) -> Result<Vec<OrderLineItemRow>, RepositoryError> {
        sqlx::query_as::<_, OrderLineItemRow>(
            "SELECT id, order_id, sku, unit_amount, quantity, line_amount \
             FROM order_line_items WHERE order_id = $1 ORDER BY id",
        )
        .bind(order_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Advance the order's payment lifecycle, guarding on `expected_version`.
    /// Attaching a Stripe payment intent that another order already holds trips
    /// the `UNIQUE` idempotency key and surfaces as an invariant violation.
    pub async fn update(
        &self,
        row: &OrderRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE orders \
             SET status = $2, total_amount = $3, stripe_payment_intent_id = $4, \
                 version = version + 1 \
             WHERE id = $1 AND version = $5",
        )
        .bind(&row.id)
        .bind(&row.status)
        .bind(row.total_amount)
        .bind(&row.stripe_payment_intent_id)
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

/// A row of `card_packs`: a purchasable pack of cards from an expansion.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CardPackRow {
    /// Aggregate identity.
    pub id: String,
    /// The expansion the pack draws from (FK).
    pub expansion_set_id: String,
    /// Pack name.
    pub name: String,
    /// Price in minor units (non-negative).
    pub price_amount: i64,
    /// Number of cards revealed on opening (strictly positive).
    pub card_count: i32,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `CardPack` aggregate.
#[derive(Debug, Clone)]
pub struct CardPackRepository {
    pool: PgPool,
}

impl CardPackRepository {
    const AGGREGATE: &'static str = "CardPack";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Register a purchasable pack.
    pub async fn insert(&self, row: &CardPackRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO card_packs (id, expansion_set_id, name, price_amount, card_count, version) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&row.id)
        .bind(&row.expansion_set_id)
        .bind(&row.name)
        .bind(row.price_amount)
        .bind(row.card_count)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a pack by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<CardPackRow>, RepositoryError> {
        sqlx::query_as::<_, CardPackRow>(
            "SELECT id, expansion_set_id, name, price_amount, card_count, version \
             FROM card_packs WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Open a pack: atomically grant every revealed card into the player's
    /// collection, bumping the collection `version` (guarded on
    /// `expected_collection_version`).
    ///
    /// Opening spans the aggregate root and one ledger row per revealed card, so
    /// it runs in a single transaction. If any card would exceed its copy cap
    /// the reveal rolls back whole — a pack never lands "half opened", and the
    /// version bump is undone with it.
    pub async fn open_pack(
        &self,
        collection_id: &str,
        expected_collection_version: i64,
        revealed: &[GrantLine],
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
        .bind(expected_collection_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;

        if bumped.rows_affected() == 0 {
            let err = conflict_or_missing(
                &mut *tx,
                Self::AGGREGATE,
                "player_collections",
                collection_id,
                expected_collection_version,
            )
            .await;
            return Err(err);
        }

        for card in revealed {
            sqlx::query(
                "INSERT INTO player_collection_cards \
                 (collection_id, card_definition_id, quantity, max_copies) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (collection_id, card_definition_id) DO UPDATE \
                 SET quantity = player_collection_cards.quantity + EXCLUDED.quantity, \
                     max_copies = EXCLUDED.max_copies",
            )
            .bind(collection_id)
            .bind(&card.card_definition_id)
            .bind(card.quantity)
            .bind(card.max_copies)
            .execute(&mut *tx)
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        }

        tx.commit()
            .await
            .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }
}

/// A row of `battle_passes`: a season-scoped progression track.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct BattlePassRow {
    /// Aggregate identity.
    pub id: String,
    /// The season this pass belongs to (unique FK — one pass per season).
    pub season_id: String,
    /// Pass name.
    pub name: String,
    /// Number of tiers (strictly positive).
    pub tier_count: i32,
    /// Purchase price in minor units (non-negative).
    pub price_amount: i64,
    /// Optimistic-concurrency version.
    pub version: i64,
}

/// PostgreSQL adapter for the `BattlePass` aggregate.
#[derive(Debug, Clone)]
pub struct BattlePassRepository {
    pool: PgPool,
}

impl BattlePassRepository {
    const AGGREGATE: &'static str = "BattlePass";
    const TABLE: &'static str = "battle_passes";

    /// Bind the adapter to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Register a battle pass for a season.
    pub async fn insert(&self, row: &BattlePassRow) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO battle_passes (id, season_id, name, tier_count, price_amount, version) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&row.id)
        .bind(&row.season_id)
        .bind(&row.name)
        .bind(row.tier_count)
        .bind(row.price_amount)
        .bind(row.version)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))?;
        Ok(())
    }

    /// Load a battle pass by identity.
    pub async fn find_by_id(&self, id: &str) -> Result<Option<BattlePassRow>, RepositoryError> {
        sqlx::query_as::<_, BattlePassRow>(
            "SELECT id, season_id, name, tier_count, price_amount, version \
             FROM battle_passes WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::classify(Self::AGGREGATE, e))
    }

    /// Amend a battle pass, guarding on `expected_version`.
    pub async fn update(
        &self,
        row: &BattlePassRow,
        expected_version: i64,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE battle_passes SET name = $2, tier_count = $3, price_amount = $4, \
             version = version + 1 WHERE id = $1 AND version = $5",
        )
        .bind(&row.id)
        .bind(&row.name)
        .bind(row.tier_count)
        .bind(row.price_amount)
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
    use crate::repositories::collection::{PlayerCollectionRepository, PlayerCollectionRow};
    use crate::repositories::content::{
        CardDefinitionRepository, CardDefinitionRow, ExpansionSetRepository, ExpansionSetRow,
    };

    fn order(id: &str) -> OrderRow {
        OrderRow {
            id: id.to_string(),
            player_id: "player-1".to_string(),
            currency: "USD".to_string(),
            total_amount: 999,
            status: "Created".to_string(),
            stripe_payment_intent_id: None,
            version: 0,
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn order_and_line_items_commit_atomically(pool: PgPool) {
        let repo = OrderRepository::new(pool);
        let ord = order("order-1");
        let items = vec![
            OrderLineItemRow {
                id: "li-1".to_string(),
                order_id: "order-1".to_string(),
                sku: "pack.base".to_string(),
                unit_amount: 499,
                quantity: 1,
                line_amount: 499,
            },
            OrderLineItemRow {
                id: "li-2".to_string(),
                order_id: "order-1".to_string(),
                sku: "pass.s1".to_string(),
                unit_amount: 500,
                quantity: 1,
                line_amount: 500,
            },
        ];
        repo.create_order(&ord, &items).await.unwrap();

        assert_eq!(repo.find_by_id("order-1").await.unwrap().unwrap(), ord);
        assert_eq!(repo.find_line_items("order-1").await.unwrap(), items);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn order_with_a_bad_line_rolls_back_the_header(pool: PgPool) {
        let repo = OrderRepository::new(pool);
        let ord = order("order-2");
        // quantity 0 violates the order_line_items quantity > 0 CHECK.
        let bad = vec![OrderLineItemRow {
            id: "li-3".to_string(),
            order_id: "order-2".to_string(),
            sku: "bad".to_string(),
            unit_amount: 100,
            quantity: 0,
            line_amount: 0,
        }];
        let err = repo.create_order(&ord, &bad).await.unwrap_err();
        assert!(
            err.is_invariant_violation(),
            "expected CHECK violation, got {err:?}"
        );
        // The header must not survive the rolled-back transaction.
        assert!(repo.find_by_id("order-2").await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn opening_a_pack_grants_revealed_cards_transactionally(pool: PgPool) {
        // Seed an expansion, two cards, and an empty collection.
        ExpansionSetRepository::new(pool.clone())
            .insert(&ExpansionSetRow {
                id: "exp-pack".to_string(),
                code: "PACK".to_string(),
                name: "Pack Set".to_string(),
                version: 0,
            })
            .await
            .unwrap();
        for (cid, rarity) in [("pc-a", "Common"), ("pc-b", "Rare")] {
            CardDefinitionRepository::new(pool.clone())
                .insert(&CardDefinitionRow {
                    id: cid.to_string(),
                    expansion_set_id: "exp-pack".to_string(),
                    name: cid.to_string(),
                    rarity: rarity.to_string(),
                    cost: 1,
                    effect_ref: None,
                    version: 0,
                })
                .await
                .unwrap();
        }
        let collections = PlayerCollectionRepository::new(pool.clone());
        collections
            .insert(&PlayerCollectionRow {
                id: "col-pack".to_string(),
                player_id: "buyer".to_string(),
                version: 0,
            })
            .await
            .unwrap();

        let packs = CardPackRepository::new(pool);
        packs
            .open_pack(
                "col-pack",
                0,
                &[GrantLine::new("pc-a", 1, 3), GrantLine::new("pc-b", 2, 3)],
            )
            .await
            .unwrap();

        let cards = collections.find_cards("col-pack").await.unwrap();
        assert_eq!(cards.len(), 2);
        assert_eq!(
            collections
                .find_by_id("col-pack")
                .await
                .unwrap()
                .unwrap()
                .version,
            1
        );
    }
}
