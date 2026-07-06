//! `/v1` routes for the shop-and-payments context: placing an order (with its
//! line items), reading an order or a card pack, opening a pack into a
//! collection, and reading a battle pass.
//!
//! The one bit of arithmetic here — `line_amount = unit_amount * quantity` and
//! the order total — is the "application-computed" derivation the persistence
//! row types document, i.e. DTO mapping the adapter contract requires, not a
//! pricing rule. Payment lifecycle, idempotency, and the copy-cap on an opened
//! pack all remain in the adapter/database.

use actix_web::{web, HttpResponse};
use serde::{Deserialize, Serialize};

use persistence::repositories::collection::PlayerCollectionRepository;
use persistence::repositories::shop::{
    BattlePassRepository, CardPackRepository, OrderLineItemRow, OrderRepository, OrderRow,
};
use persistence::repositories::GrantLine;

use super::collection::GrantLineRequest;
use super::envelope::{created, ok, ApiError};
use super::identity::Identity;
use super::validate::Validator;
use super::ApiState;

/// Register the shop routes onto the parent `/v1` scope.
pub fn routes(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/orders").route(web::post().to(create_order)))
        .service(web::resource("/orders/{id}").route(web::get().to(get_order)))
        .service(web::resource("/card-packs/{id}").route(web::get().to(get_card_pack)))
        .service(web::resource("/card-packs/{id}/open").route(web::post().to(open_pack)))
        .service(web::resource("/battle-passes/{id}").route(web::get().to(get_battle_pass)));
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// One requested line of a `POST /orders` order.
#[derive(Debug, Deserialize)]
struct LineItemRequest {
    /// Line identity.
    id: String,
    /// Stock-keeping unit purchased.
    sku: String,
    /// Per-unit price in minor units (non-negative).
    unit_amount: i64,
    /// Quantity purchased (strictly positive).
    quantity: i32,
}

/// Body of `POST /orders`. The purchasing player is taken from the identity
/// header, and both the per-line and order totals are computed server-side.
#[derive(Debug, Deserialize)]
struct CreateOrderRequest {
    /// Client-chosen aggregate identity for the order.
    id: String,
    /// ISO-4217 currency code (exactly three letters).
    currency: String,
    /// The order's line items (at least one).
    items: Vec<LineItemRequest>,
}

/// Body of `POST /card-packs/{id}/open` — reveal a pack into a collection.
#[derive(Debug, Deserialize)]
struct OpenPackRequest {
    /// The collection the revealed cards land in.
    collection_id: String,
    /// The collection version the caller believes is current (optimistic lock).
    expected_collection_version: i64,
    /// The cards the pack reveals.
    revealed: Vec<GrantLineRequest>,
}

/// One order line item on the wire.
#[derive(Debug, Serialize)]
struct OrderLineItem {
    id: String,
    sku: String,
    unit_amount: i64,
    quantity: i32,
    line_amount: i64,
}

/// An order (header + line items) on the wire.
#[derive(Debug, Serialize)]
struct OrderResponse {
    id: String,
    player_id: String,
    currency: String,
    total_amount: i64,
    status: String,
    stripe_payment_intent_id: Option<String>,
    version: i64,
    items: Vec<OrderLineItem>,
}

/// A card pack on the wire.
#[derive(Debug, Serialize)]
struct CardPackResponse {
    id: String,
    expansion_set_id: String,
    name: String,
    price_amount: i64,
    card_count: i32,
    version: i64,
}

/// A battle pass on the wire.
#[derive(Debug, Serialize)]
struct BattlePassResponse {
    id: String,
    season_id: String,
    name: String,
    tier_count: i32,
    price_amount: i64,
    version: i64,
}

/// One owned-card balance echoed back after opening a pack.
#[derive(Debug, Serialize)]
struct OwnedCard {
    card_definition_id: String,
    quantity: i32,
    max_copies: i32,
}

/// The collection state after a pack open.
#[derive(Debug, Serialize)]
struct CollectionResponse {
    id: String,
    player_id: String,
    version: i64,
    cards: Vec<OwnedCard>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /orders` — place an order together with its line items, atomically.
async fn create_order(
    state: web::Data<ApiState>,
    identity: Identity,
    body: web::Json<CreateOrderRequest>,
) -> Result<HttpResponse, ApiError> {
    let body = body.into_inner();

    let mut v = Validator::new();
    v.non_empty("id", &body.id);
    v.require(
        body.currency.len() == 3,
        "currency",
        "must be a 3-letter ISO-4217 code",
    );
    v.require(!body.items.is_empty(), "items", "must not be empty");
    for (i, item) in body.items.iter().enumerate() {
        let p = format!("items[{i}]");
        v.non_empty(&format!("{p}.id"), &item.id);
        v.non_empty(&format!("{p}.sku"), &item.sku);
        v.require(
            item.unit_amount >= 0,
            &format!("{p}.unit_amount"),
            "must not be negative",
        );
        v.require(
            item.quantity > 0,
            &format!("{p}.quantity"),
            "must be greater than zero",
        );
    }
    v.finish()?;

    identity.audit("order.create");
    // Derive each line total and the order total (the adapter's documented
    // application-computed fields) — arithmetic, not a pricing rule.
    let items: Vec<OrderLineItemRow> = body
        .items
        .iter()
        .map(|item| OrderLineItemRow {
            id: item.id.clone(),
            order_id: body.id.clone(),
            sku: item.sku.clone(),
            unit_amount: item.unit_amount,
            quantity: item.quantity,
            line_amount: item.unit_amount * i64::from(item.quantity),
        })
        .collect();
    let total_amount: i64 = items.iter().map(|i| i.line_amount).sum();

    let order = OrderRow {
        id: body.id.clone(),
        // Purchaser comes from the trusted identity, not the request body.
        player_id: identity.player_id,
        currency: body.currency,
        total_amount,
        status: "Created".to_string(),
        stripe_payment_intent_id: None,
        version: 0,
    };

    let repo = OrderRepository::new(state.pool.clone());
    repo.create_order(&order, &items).await?;

    Ok(created(load_order(&repo, &body.id).await?))
}

/// Read an order header + its line items, or 404, as a response DTO.
async fn load_order(repo: &OrderRepository, id: &str) -> Result<OrderResponse, ApiError> {
    let row = repo.find_by_id(id).await?.ok_or(ApiError::NotFound {
        resource: "Order",
        id: id.to_string(),
    })?;
    let items = repo.find_line_items(id).await?;
    Ok(OrderResponse {
        id: row.id,
        player_id: row.player_id,
        currency: row.currency,
        total_amount: row.total_amount,
        status: row.status,
        stripe_payment_intent_id: row.stripe_payment_intent_id,
        version: row.version,
        items: items
            .into_iter()
            .map(|i| OrderLineItem {
                id: i.id,
                sku: i.sku,
                unit_amount: i.unit_amount,
                quantity: i.quantity,
                line_amount: i.line_amount,
            })
            .collect(),
    })
}

/// `GET /orders/{id}` — read an order and its line items.
async fn get_order(
    state: web::Data<ApiState>,
    identity: Identity,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let repo = OrderRepository::new(state.pool.clone());
    let resp = load_order(&repo, &id).await?;
    // Object-level authorization: a caller may only read their own order.
    // NotFound (not Forbidden) so a client-guessable order id cannot be probed.
    identity.require_owner(&resp.player_id, "Order", &id)?;
    Ok(ok(resp))
}

/// `GET /card-packs/{id}` — read a purchasable pack.
async fn get_card_pack(
    state: web::Data<ApiState>,
    _identity: Identity,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let row = CardPackRepository::new(state.pool.clone())
        .find_by_id(&id)
        .await?
        .ok_or(ApiError::NotFound {
            resource: "CardPack",
            id: id.clone(),
        })?;
    Ok(ok(CardPackResponse {
        id: row.id,
        expansion_set_id: row.expansion_set_id,
        name: row.name,
        price_amount: row.price_amount,
        card_count: row.card_count,
        version: row.version,
    }))
}

/// `POST /card-packs/{id}/open` — reveal a pack's cards into a collection.
///
/// Privileged and server-authoritative: the revealed cards are supplied in the
/// request, so a player-callable version would let anyone mint arbitrary cards
/// into their collection. It is therefore restricted to internal service
/// accounts (fulfillment). In the target design a player buys a pack (an order),
/// and the fulfillment service — holding the `service` role — computes the
/// reveal from the pack definition and calls this; that reveal logic is a
/// domain concern not yet implemented (the aggregates are still stubs), so this
/// handler deliberately does not fabricate it here.
async fn open_pack(
    state: web::Data<ApiState>,
    identity: Identity,
    _path: web::Path<String>,
    body: web::Json<OpenPackRequest>,
) -> Result<HttpResponse, ApiError> {
    let body = body.into_inner();

    identity.require_service("card_pack.open")?;

    let mut v = Validator::new();
    v.non_empty("collection_id", &body.collection_id);
    v.require(!body.revealed.is_empty(), "revealed", "must not be empty");
    for (i, line) in body.revealed.iter().enumerate() {
        line.validate(&mut v, &format!("revealed[{i}]"));
    }
    v.finish()?;

    identity.audit("card_pack.open");
    let revealed: Vec<GrantLine> = body
        .revealed
        .iter()
        .map(GrantLineRequest::to_grant_line)
        .collect();
    CardPackRepository::new(state.pool.clone())
        .open_pack(
            &body.collection_id,
            body.expected_collection_version,
            &revealed,
        )
        .await?;

    // Echo the resulting collection so the client sees the new balances.
    let collections = PlayerCollectionRepository::new(state.pool.clone());
    let root = collections
        .find_by_id(&body.collection_id)
        .await?
        .ok_or(ApiError::NotFound {
            resource: "PlayerCollection",
            id: body.collection_id.clone(),
        })?;
    let cards = collections.find_cards(&body.collection_id).await?;
    Ok(ok(CollectionResponse {
        id: root.id,
        player_id: root.player_id,
        version: root.version,
        cards: cards
            .into_iter()
            .map(|c| OwnedCard {
                card_definition_id: c.card_definition_id,
                quantity: c.quantity,
                max_copies: c.max_copies,
            })
            .collect(),
    }))
}

/// `GET /battle-passes/{id}` — read a battle pass.
async fn get_battle_pass(
    state: web::Data<ApiState>,
    _identity: Identity,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let row = BattlePassRepository::new(state.pool.clone())
        .find_by_id(&id)
        .await?
        .ok_or(ApiError::NotFound {
            resource: "BattlePass",
            id: id.clone(),
        })?;
    Ok(ok(BattlePassResponse {
        id: row.id,
        season_id: row.season_id,
        name: row.name,
        tier_count: row.tier_count,
        price_amount: row.price_amount,
        version: row.version,
    }))
}
