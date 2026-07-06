//! `/v1` read routes for the content catalog: expansion sets, card definitions,
//! and boss definitions. All are by-identity reads that map a catalog row to a
//! response DTO.

use actix_web::{web, HttpResponse};
use serde::Serialize;

use persistence::repositories::content::{
    BossDefinitionRepository, CardDefinitionRepository, ExpansionSetRepository,
};

use super::envelope::{ok, ApiError};
use super::identity::Identity;
use super::ApiState;

/// Register the catalog read routes onto the parent `/v1` scope.
pub fn routes(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/catalog/expansions/{id}").route(web::get().to(get_expansion)))
        .service(web::resource("/catalog/cards/{id}").route(web::get().to(get_card)))
        .service(web::resource("/catalog/bosses/{id}").route(web::get().to(get_boss)));
}

/// An expansion set on the wire.
#[derive(Debug, Serialize)]
struct ExpansionResponse {
    id: String,
    code: String,
    name: String,
    version: i64,
}

/// A card definition on the wire.
#[derive(Debug, Serialize)]
struct CardResponse {
    id: String,
    expansion_set_id: String,
    name: String,
    rarity: String,
    cost: i32,
    effect_ref: Option<String>,
    version: i64,
}

/// A boss definition on the wire.
#[derive(Debug, Serialize)]
struct BossResponse {
    id: String,
    expansion_set_id: String,
    name: String,
    version: i64,
}

/// `GET /catalog/expansions/{id}` — read an expansion set.
async fn get_expansion(
    state: web::Data<ApiState>,
    _identity: Identity,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let row = ExpansionSetRepository::new(state.pool.clone())
        .find_by_id(&id)
        .await?
        .ok_or(ApiError::NotFound {
            resource: "ExpansionSet",
            id: id.clone(),
        })?;
    Ok(ok(ExpansionResponse {
        id: row.id,
        code: row.code,
        name: row.name,
        version: row.version,
    }))
}

/// `GET /catalog/cards/{id}` — read a card definition.
async fn get_card(
    state: web::Data<ApiState>,
    _identity: Identity,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let row = CardDefinitionRepository::new(state.pool.clone())
        .find_by_id(&id)
        .await?
        .ok_or(ApiError::NotFound {
            resource: "CardDefinition",
            id: id.clone(),
        })?;
    Ok(ok(CardResponse {
        id: row.id,
        expansion_set_id: row.expansion_set_id,
        name: row.name,
        rarity: row.rarity,
        cost: row.cost,
        effect_ref: row.effect_ref,
        version: row.version,
    }))
}

/// `GET /catalog/bosses/{id}` — read a boss definition.
async fn get_boss(
    state: web::Data<ApiState>,
    _identity: Identity,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let row = BossDefinitionRepository::new(state.pool.clone())
        .find_by_id(&id)
        .await?
        .ok_or(ApiError::NotFound {
            resource: "BossDefinition",
            id: id.clone(),
        })?;
    Ok(ok(BossResponse {
        id: row.id,
        expansion_set_id: row.expansion_set_id,
        name: row.name,
        version: row.version,
    }))
}
