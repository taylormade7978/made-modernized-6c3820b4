//! `/v1` read routes for the matchmaking-and-ranked context: the season
//! leaderboard, a single player's ranked standing, and season lookups.
//!
//! These are pure reads. The leaderboard is served by the crate's covering-index
//! query (`persistence::leaderboard`); the per-standing and per-season reads go
//! through their repository adapters. Handlers only clamp the page size and map
//! rows to DTOs.

use actix_web::{web, HttpResponse};
use serde::{Deserialize, Serialize};

use persistence::repositories::matchmaking::{RankedStandingRepository, SeasonRepository};

use super::envelope::{ok, ApiError};
use super::identity::Identity;
use super::ApiState;

/// Default leaderboard page size when the caller does not specify `limit`.
const DEFAULT_LIMIT: i64 = 100;
/// Hard cap on the leaderboard page size, so a caller cannot ask for an
/// unbounded scan.
const MAX_LIMIT: i64 = 500;

/// Register the leaderboard/ranked read routes onto the parent `/v1` scope.
pub fn routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::resource("/seasons/{season_id}/leaderboard").route(web::get().to(get_leaderboard)),
    )
    .service(web::resource("/seasons/{id}").route(web::get().to(get_season)))
    .service(web::resource("/ranked-standings/{id}").route(web::get().to(get_ranked_standing)));
}

/// Query string for `GET /seasons/{season_id}/leaderboard`.
#[derive(Debug, Deserialize)]
struct LeaderboardQuery {
    /// Page size; defaults to [`DEFAULT_LIMIT`], clamped to [`MAX_LIMIT`].
    limit: Option<i64>,
}

/// One leaderboard row on the wire.
#[derive(Debug, Serialize)]
struct LeaderboardEntry {
    player_id: String,
    rating: f64,
    tier: String,
    stars: i16,
}

/// Response for a leaderboard page.
#[derive(Debug, Serialize)]
struct LeaderboardResponse {
    season_id: String,
    limit: i64,
    entries: Vec<LeaderboardEntry>,
}

/// A ranked standing on the wire.
#[derive(Debug, Serialize)]
struct RankedStandingResponse {
    id: String,
    player_id: String,
    season_id: String,
    rating: f64,
    rating_dev: f64,
    volatility: f64,
    tier: String,
    stars: i16,
    floor_tier: String,
    matches_played: i32,
    version: i64,
}

/// A season on the wire.
#[derive(Debug, Serialize)]
struct SeasonResponse {
    id: String,
    number: i32,
    name: String,
    version: i64,
}

/// `GET /seasons/{season_id}/leaderboard?limit=` — the top standings by rating.
async fn get_leaderboard(
    state: web::Data<ApiState>,
    _identity: Identity,
    path: web::Path<String>,
    query: web::Query<LeaderboardQuery>,
) -> Result<HttpResponse, ApiError> {
    let season_id = path.into_inner();
    // Clamp the requested page size into (0, MAX_LIMIT]; a non-positive request
    // falls back to the default rather than erroring.
    let limit = match query.limit {
        Some(n) if n > 0 => n.min(MAX_LIMIT),
        _ => DEFAULT_LIMIT,
    };

    let rows = persistence::leaderboard(&state.pool, &season_id, limit)
        .await
        .map_err(|e| ApiError::from(persistence::RepositoryError::from(e)))?;

    Ok(ok(LeaderboardResponse {
        season_id,
        limit,
        entries: rows
            .into_iter()
            .map(|r| LeaderboardEntry {
                player_id: r.player_id,
                rating: r.rating,
                tier: r.tier,
                stars: r.stars,
            })
            .collect(),
    }))
}

/// `GET /ranked-standings/{id}` — one player's standing for a season.
async fn get_ranked_standing(
    state: web::Data<ApiState>,
    _identity: Identity,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let row = RankedStandingRepository::new(state.pool.clone())
        .find_by_id(&id)
        .await?
        .ok_or(ApiError::NotFound {
            resource: "RankedStanding",
            id: id.clone(),
        })?;
    Ok(ok(RankedStandingResponse {
        id: row.id,
        player_id: row.player_id,
        season_id: row.season_id,
        rating: row.rating,
        rating_dev: row.rating_dev,
        volatility: row.volatility,
        tier: row.tier,
        stars: row.stars,
        floor_tier: row.floor_tier,
        matches_played: row.matches_played,
        version: row.version,
    }))
}

/// `GET /seasons/{id}` — a single season header.
async fn get_season(
    state: web::Data<ApiState>,
    _identity: Identity,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = path.into_inner();
    let row = SeasonRepository::new(state.pool.clone())
        .find_by_id(&id)
        .await?
        .ok_or(ApiError::NotFound {
            resource: "Season",
            id: id.clone(),
        })?;
    Ok(ok(SeasonResponse {
        id: row.id,
        number: row.number,
        name: row.name,
        version: row.version,
    }))
}
