//! The `/v1` REST API — the HTTP driving adapter over the Postgres repository
//! adapters.
//!
//! This module is the seam the story (S-76) adds: actix-web routes and DTO
//! mapping for the collection-deck, leaderboard/ranked, shop-payments, and
//! content-catalog contexts. It deliberately holds **no** business rules — each
//! handler decodes a request DTO, validates its *shape*, reads the trusted
//! identity from gateway headers, delegates to a [`persistence`] repository
//! adapter, and maps the result back to a response DTO through the shared
//! [`envelope`]. Auth is handled entirely by the Kong/OPA sidecars upstream, so
//! there is no auth middleware here (see [`identity`]).
//!
//! Wiring is two calls from `main`: register [`json_config`] as app data (so a
//! malformed body renders the same structured 400 envelope as a failed
//! validation), then [`configure`] the `/v1` scope.

use actix_web::error::JsonPayloadError;
use actix_web::{web, HttpRequest};

use persistence::PgPool;

pub mod catalog;
pub mod collection;
pub mod envelope;
pub mod identity;
pub mod leaderboard;
pub mod shop;
pub mod validate;

use envelope::{ApiError, FieldError};

/// Shared REST application state: the connection pool every repository adapter
/// is constructed over. The pool is cheap to clone (an `Arc` inside), so
/// handlers build a fresh adapter per request from `state.pool.clone()`.
#[derive(Clone)]
pub struct ApiState {
    /// The lazily-connected Postgres pool backing every `/v1` read and write.
    pub pool: PgPool,
}

impl ApiState {
    /// Bind the REST state to a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// A [`web::JsonConfig`] whose error handler renders a malformed or wrong-typed
/// request body as an [`ApiError::Validation`] — i.e. the *same* structured 400
/// envelope a failed field validation produces, instead of actix's default
/// plain-text 400. This is what makes "return 400 with a structured error body
/// on malformed payloads" hold uniformly.
pub fn json_config() -> web::JsonConfig {
    web::JsonConfig::default().error_handler(|err: JsonPayloadError, _req: &HttpRequest| {
        ApiError::Validation(vec![FieldError {
            field: "body".to_string(),
            message: err.to_string(),
        }])
        .into()
    })
}

/// Mount every context's routes under a single `/v1` scope.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/v1")
            .configure(collection::routes)
            .configure(leaderboard::routes)
            .configure(shop::routes)
            .configure(catalog::routes),
    );
}
