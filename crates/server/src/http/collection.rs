//! `/v1` routes for the collection-and-deckbuilding context: a player's owned
//! card collection (with its grant ledger) and their outfits (decks).
//!
//! Every handler is a thin translation layer — decode a DTO, validate its
//! shape, take the owning player from the trusted [`Identity`] header (never the
//! body), delegate to a [`persistence`] repository adapter, and map the result
//! row back to a response DTO. No copy caps, ledger arithmetic, or version
//! bookkeeping live here; those are the adapter's and the database's job.

use actix_web::{web, HttpResponse};
use serde::{Deserialize, Serialize};

use persistence::repositories::collection::{
    OutfitRepository, OutfitRow, PlayerCollectionRepository, PlayerCollectionRow,
};
use persistence::repositories::GrantLine;

use super::envelope::{created, ok, ApiError};
use super::identity::Identity;
use super::validate::Validator;
use super::ApiState;

/// Register the collection/outfit routes onto the parent `/v1` scope.
pub fn routes(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/collections").route(web::post().to(create_collection)))
        .service(web::resource("/collections/{id}").route(web::get().to(get_collection)))
        .service(web::resource("/collections/{id}/grants").route(web::post().to(grant_cards)))
        .service(web::resource("/outfits").route(web::post().to(create_outfit)))
        .service(
            web::resource("/outfits/{id}")
                .route(web::get().to(get_outfit))
                .route(web::put().to(update_outfit)),
        );
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Body of `POST /collections`. The owning player is *not* here — it is taken
/// from the caller's identity header so a client cannot open a collection for
/// someone else.
#[derive(Debug, Deserialize)]
struct CreateCollectionRequest {
    /// Client-chosen aggregate identity for the new collection.
    id: String,
}

/// One card grant in a `POST /collections/{id}/grants` request. Shared with the
/// shop's pack-opening endpoint.
#[derive(Debug, Deserialize)]
pub struct GrantLineRequest {
    /// The catalog card being granted.
    pub card_definition_id: String,
    /// How many copies to add (must be positive).
    pub quantity: i32,
    /// The per-card copy cap the ledger `CHECK` enforces (positive).
    pub max_copies: i32,
}

impl GrantLineRequest {
    /// Validate this line against `prefix` (e.g. `"grants[0]"`).
    pub fn validate(&self, v: &mut Validator, prefix: &str) {
        v.non_empty(
            &format!("{prefix}.card_definition_id"),
            &self.card_definition_id,
        );
        v.require(
            self.quantity > 0,
            &format!("{prefix}.quantity"),
            "must be greater than zero",
        );
        v.require(
            self.max_copies > 0,
            &format!("{prefix}.max_copies"),
            "must be greater than zero",
        );
    }

    /// Map to the adapter's [`GrantLine`] value.
    pub fn to_grant_line(&self) -> GrantLine {
        GrantLine::new(
            self.card_definition_id.clone(),
            self.quantity,
            self.max_copies,
        )
    }
}

/// Body of `POST /collections/{id}/grants`.
#[derive(Debug, Deserialize)]
struct GrantCardsRequest {
    /// The collection version the caller believes is current (optimistic lock).
    expected_version: i64,
    /// The cards to grant.
    grants: Vec<GrantLineRequest>,
}

/// Body of `POST /outfits`.
#[derive(Debug, Deserialize)]
struct CreateOutfitRequest {
    /// Client-chosen aggregate identity for the new outfit.
    id: String,
    /// The collection this outfit draws cards from.
    collection_id: String,
    /// Outfit (deck) name.
    name: String,
}

/// Body of `PUT /outfits/{id}` — a rename guarded by the expected version.
#[derive(Debug, Deserialize)]
struct UpdateOutfitRequest {
    /// New outfit name.
    name: String,
    /// The outfit version the caller believes is current (optimistic lock).
    expected_version: i64,
}

/// One owned-card balance in a collection response.
#[derive(Debug, Serialize)]
struct OwnedCard {
    card_definition_id: String,
    quantity: i32,
    max_copies: i32,
}

/// Response shape for a collection: the root plus its owned-card ledger.
#[derive(Debug, Serialize)]
struct CollectionResponse {
    id: String,
    player_id: String,
    version: i64,
    cards: Vec<OwnedCard>,
}

/// Response shape for an outfit.
#[derive(Debug, Serialize)]
struct OutfitResponse {
    id: String,
    player_id: String,
    collection_id: String,
    name: String,
    version: i64,
}

impl From<OutfitRow> for OutfitResponse {
    fn from(row: OutfitRow) -> Self {
        OutfitResponse {
            id: row.id,
            player_id: row.player_id,
            collection_id: row.collection_id,
            name: row.name,
            version: row.version,
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Read a collection root and its ledger, or 404, as a response DTO.
async fn load_collection(
    repo: &PlayerCollectionRepository,
    id: &str,
) -> Result<CollectionResponse, ApiError> {
    let row = repo.find_by_id(id).await?.ok_or(ApiError::NotFound {
        resource: "PlayerCollection",
        id: id.to_string(),
    })?;
    let cards = repo.find_cards(id).await?;
    Ok(CollectionResponse {
        id: row.id,
        player_id: row.player_id,
        version: row.version,
        cards: cards
            .into_iter()
            .map(|c| OwnedCard {
                card_definition_id: c.card_definition_id,
                quantity: c.quantity,
                max_copies: c.max_copies,
            })
            .collect(),
    })
}

/// `POST /collections` — open an empty collection owned by the caller.
async fn create_collection(
    state: web::Data<ApiState>,
    identity: Identity,
    body: web::Json<CreateCollectionRequest>,
) -> Result<HttpResponse, ApiError> {
    let body = body.into_inner();
    let mut v = Validator::new();
    v.non_empty("id", &body.id);
    v.finish()?;

    identity.audit("collection.create");
    let repo = PlayerCollectionRepository::new(state.pool.clone());
    repo.insert(&PlayerCollectionRow {
        id: body.id.clone(),
        // Owner comes from the trusted identity, not the request body.
        player_id: identity.player_id,
        version: 0,
    })
    .await?;

    Ok(created(load_collection(&repo, &body.id).await?))
}

/// `GET /collections/{id}` — read a collection and its owned-card ledger.
async fn get_collection(
    state: web::Data<ApiState>,
    identity: Identity,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let repo = PlayerCollectionRepository::new(state.pool.clone());
    let resp = load_collection(&repo, &id).await?;
    // Object-level authorization: a caller may only read their own collection.
    identity.require_owner(&resp.player_id, "PlayerCollection", &id)?;
    Ok(ok(resp))
}

/// `POST /collections/{id}/grants` — atomically grant cards into the ledger.
///
/// This is a privileged, server-authoritative operation: the *contents* of the
/// grant are supplied by the caller, so exposing it to players would let anyone
/// mint themselves arbitrary cards. It is therefore restricted to internal
/// service accounts (fulfillment, rewards). Player-facing card acquisition must
/// go through a purchase/reward flow that calls this with a service identity
/// once the domain reveal/entitlement logic exists.
async fn grant_cards(
    state: web::Data<ApiState>,
    identity: Identity,
    path: web::Path<String>,
    body: web::Json<GrantCardsRequest>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let body = body.into_inner();

    identity.require_service("collection.grant")?;

    let mut v = Validator::new();
    v.require(!body.grants.is_empty(), "grants", "must not be empty");
    for (i, line) in body.grants.iter().enumerate() {
        line.validate(&mut v, &format!("grants[{i}]"));
    }
    v.finish()?;

    identity.audit("collection.grant");
    let grants: Vec<GrantLine> = body
        .grants
        .iter()
        .map(GrantLineRequest::to_grant_line)
        .collect();
    let repo = PlayerCollectionRepository::new(state.pool.clone());
    repo.grant_cards(&id, body.expected_version, &grants)
        .await?;

    Ok(ok(load_collection(&repo, &id).await?))
}

/// `POST /outfits` — create a new outfit (deck) owned by the caller.
async fn create_outfit(
    state: web::Data<ApiState>,
    identity: Identity,
    body: web::Json<CreateOutfitRequest>,
) -> Result<HttpResponse, ApiError> {
    let body = body.into_inner();
    let mut v = Validator::new();
    v.non_empty("id", &body.id);
    v.non_empty("collection_id", &body.collection_id);
    v.non_empty("name", &body.name);
    v.finish()?;

    // The outfit must draw from a collection the caller owns — otherwise a
    // player could build a deck against another player's collection.
    let collections = PlayerCollectionRepository::new(state.pool.clone());
    let coll = collections
        .find_by_id(&body.collection_id)
        .await?
        .ok_or(ApiError::NotFound {
            resource: "PlayerCollection",
            id: body.collection_id.clone(),
        })?;
    identity.require_owner(&coll.player_id, "PlayerCollection", &body.collection_id)?;

    identity.audit("outfit.create");
    let row = OutfitRow {
        id: body.id,
        // Owner comes from the trusted identity, not the request body.
        player_id: identity.player_id,
        collection_id: body.collection_id,
        name: body.name,
        version: 0,
    };
    let repo = OutfitRepository::new(state.pool.clone());
    repo.insert(&row).await?;

    Ok(created(OutfitResponse::from(row)))
}

/// `GET /outfits/{id}` — read a single outfit.
async fn get_outfit(
    state: web::Data<ApiState>,
    identity: Identity,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let repo = OutfitRepository::new(state.pool.clone());
    let row = repo.find_by_id(&id).await?.ok_or(ApiError::NotFound {
        resource: "Outfit",
        id: id.clone(),
    })?;
    // Object-level authorization: only the owner may read the outfit.
    identity.require_owner(&row.player_id, "Outfit", &id)?;
    Ok(ok(OutfitResponse::from(row)))
}

/// `PUT /outfits/{id}` — rename an outfit under optimistic concurrency.
async fn update_outfit(
    state: web::Data<ApiState>,
    identity: Identity,
    path: web::Path<String>,
    body: web::Json<UpdateOutfitRequest>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let body = body.into_inner();
    let mut v = Validator::new();
    v.non_empty("name", &body.name);
    v.finish()?;

    let repo = OutfitRepository::new(state.pool.clone());
    // Load the current row so the immutable fields (owner, collection) are
    // preserved; only the name changes. Missing → 404 from the adapter's read.
    let mut row = repo.find_by_id(&id).await?.ok_or(ApiError::NotFound {
        resource: "Outfit",
        id: id.clone(),
    })?;
    // Object-level authorization: only the owner may rename the outfit. Checked
    // before any mutation, and returns NotFound so a non-owner cannot even tell
    // the outfit exists.
    identity.require_owner(&row.player_id, "Outfit", &id)?;

    identity.audit("outfit.update");
    row.name = body.name;
    repo.update(&row, body.expected_version).await?;

    // Re-read so the response reflects the bumped version.
    let updated = repo.find_by_id(&id).await?.ok_or(ApiError::NotFound {
        resource: "Outfit",
        id,
    })?;
    Ok(ok(OutfitResponse::from(updated)))
}
