//! The consistent success/error envelope every `/v1` handler renders through,
//! and the single place HTTP status codes are decided.
//!
//! Every response body has one of two shapes:
//!
//! * success — `{ "data": <payload> }`
//! * error — `{ "error": { "code": "...", "message": "...", "details": [...] } }`
//!
//! Handlers never build an [`HttpResponse`] for a failure by hand: they return
//! `Result<HttpResponse, ApiError>`, and actix renders the error via
//! [`ApiError`]'s [`ResponseError`] impl. That keeps status-code selection in
//! exactly one table (here) instead of scattered across handlers, and it is how
//! a repository's typed failure (a stale-version [`RepositoryError::Conflict`],
//! a `CHECK`-violating [`RepositoryError::InvariantViolation`]) is translated
//! into the right HTTP status without a handler ever encoding a business rule.

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, ResponseError};
use serde::Serialize;

use persistence::RepositoryError;

/// The success envelope: a single `data` member wrapping the payload.
#[derive(Debug, Serialize)]
pub struct Envelope<T: Serialize> {
    /// The handler's payload.
    pub data: T,
}

/// One field-level validation failure, surfaced in an error's `details`.
#[derive(Debug, Serialize)]
pub struct FieldError {
    /// The offending field (request-body path or query parameter).
    pub field: String,
    /// A human-readable reason the value was rejected.
    pub message: String,
}

/// The error envelope body: a stable machine `code`, a human `message`, and an
/// optional list of per-field `details` (populated for validation failures).
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// Stable, machine-readable error code (e.g. `"validation_error"`).
    pub code: &'static str,
    /// Human-readable explanation.
    pub message: String,
    /// Field-level failures; omitted from the JSON when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<FieldError>,
}

/// The wrapper object so an error body serializes as `{ "error": { ... } }`.
#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

/// Every failure a `/v1` handler can return. The variant *is* the HTTP status:
/// the mapping lives in [`ApiError::status_code`]/[`ApiError::code`], nowhere
/// else.
#[derive(Debug)]
pub enum ApiError {
    /// Request DTO failed validation (or the JSON body was malformed) — 400.
    Validation(Vec<FieldError>),
    /// A required trusted gateway identity header was absent — 401. Auth itself
    /// is enforced by the Kong/OPA sidecars; this only guards against the
    /// header never being set (a gateway misconfiguration), it does not verify
    /// any token.
    Unauthenticated(String),
    /// No resource exists for the requested identity — 404.
    NotFound {
        /// The resource/aggregate type that was queried.
        resource: &'static str,
        /// The identity that produced no row.
        id: String,
    },
    /// The caller is authenticated but not permitted to perform this operation
    /// (e.g. a privileged, service-role-only action invoked by a player) — 403.
    /// Distinct from [`ApiError::NotFound`], which is used for object-level
    /// ownership mismatches so a caller cannot probe for another player's
    /// resource ids.
    Forbidden(String),
    /// Optimistic-concurrency loss: the caller's `expected_version` was stale — 409.
    Conflict(String),
    /// A domain/database invariant rejected the write (copy cap, non-negative
    /// balance, unique idempotency key, …) — 422.
    Unprocessable(String),
    /// An unexpected backend failure — 500. The underlying detail is logged, not
    /// leaked to the client.
    Internal(String),
}

impl ApiError {
    /// Machine-readable code for the error envelope.
    fn code(&self) -> &'static str {
        match self {
            ApiError::Validation(_) => "validation_error",
            ApiError::Unauthenticated(_) => "unauthenticated",
            ApiError::Forbidden(_) => "forbidden",
            ApiError::NotFound { .. } => "not_found",
            ApiError::Conflict(_) => "conflict",
            ApiError::Unprocessable(_) => "unprocessable_entity",
            ApiError::Internal(_) => "internal_error",
        }
    }

    /// The human-readable message and any per-field details.
    fn body(&self) -> ErrorBody {
        let (message, details) = match self {
            ApiError::Validation(fields) => (
                "request validation failed".to_string(),
                // `FieldError` is not `Clone`; rebuild the details list.
                fields
                    .iter()
                    .map(|f| FieldError {
                        field: f.field.clone(),
                        message: f.message.clone(),
                    })
                    .collect(),
            ),
            ApiError::Unauthenticated(msg) => (msg.clone(), Vec::new()),
            ApiError::Forbidden(msg) => (msg.clone(), Vec::new()),
            ApiError::NotFound { resource, id } => {
                (format!("no {resource} found for id '{id}'"), Vec::new())
            }
            ApiError::Conflict(msg) => (msg.clone(), Vec::new()),
            ApiError::Unprocessable(msg) => (msg.clone(), Vec::new()),
            ApiError::Internal(_) => ("an internal error occurred".to_string(), Vec::new()),
        };
        ErrorBody {
            code: self.code(),
            message,
            details,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Display` feeds actix's logger, not the HTTP body, so it carries the
        // full internal detail; the wire body ([`error_response`]) stays generic.
        match self {
            ApiError::Internal(detail) => write!(f, "internal_error: {detail}"),
            other => write!(f, "{}: {}", other.code(), other.body().message),
        }
    }
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            ApiError::Validation(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::NotFound { .. } => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        HttpResponse::build(self.status_code()).json(ErrorEnvelope { error: self.body() })
    }
}

impl From<RepositoryError> for ApiError {
    /// Translate a repository adapter failure into the matching HTTP error.
    /// This is the seam that lets handlers stay rule-free: the adapter already
    /// decided *what* went wrong (a lost optimistic-concurrency race, a tripped
    /// `CHECK`), and this table decides only *which status* reports it.
    fn from(err: RepositoryError) -> Self {
        match &err {
            RepositoryError::NotFound { aggregate, id } => ApiError::NotFound {
                resource: aggregate,
                id: id.clone(),
            },
            RepositoryError::Conflict { .. } => ApiError::Conflict(err.to_string()),
            RepositoryError::InvariantViolation { .. } => ApiError::Unprocessable(err.to_string()),
            RepositoryError::Database(_) => {
                // Log the real driver error server-side; never leak it to the wire.
                eprintln!("repository database error: {err}");
                ApiError::Internal(err.to_string())
            }
        }
    }
}

/// 200 OK carrying the success envelope around `data`.
pub fn ok<T: Serialize>(data: T) -> HttpResponse {
    HttpResponse::Ok().json(Envelope { data })
}

/// 201 Created carrying the success envelope around `data`.
pub fn created<T: Serialize>(data: T) -> HttpResponse {
    HttpResponse::Created().json(Envelope { data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_envelope_wraps_payload_under_data() {
        let json = serde_json::to_value(Envelope { data: 42 }).unwrap();
        assert_eq!(json, serde_json::json!({ "data": 42 }));
    }

    #[test]
    fn error_variants_map_to_the_expected_status_codes() {
        assert_eq!(
            ApiError::Validation(vec![]).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::Unauthenticated("x".into()).status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            ApiError::Forbidden("x".into()).status_code(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            ApiError::NotFound {
                resource: "Order",
                id: "o-1".into()
            }
            .status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ApiError::Conflict("x".into()).status_code(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            ApiError::Unprocessable("x".into()).status_code(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            ApiError::Internal("x".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn error_body_serializes_with_the_envelope_shape_and_omits_empty_details() {
        let body = ApiError::Conflict("stale".into()).body();
        let json = serde_json::to_value(ErrorEnvelope { error: body }).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "error": { "code": "conflict", "message": "stale" } })
        );
        // `details` is omitted entirely when empty (not rendered as `[]`).
        assert!(json["error"].get("details").is_none());
    }

    #[test]
    fn validation_error_carries_field_details() {
        let err = ApiError::Validation(vec![FieldError {
            field: "quantity".into(),
            message: "must be greater than zero".into(),
        }]);
        let json = serde_json::to_value(ErrorEnvelope { error: err.body() }).unwrap();
        assert_eq!(json["error"]["code"], "validation_error");
        assert_eq!(json["error"]["details"][0]["field"], "quantity");
    }

    #[test]
    fn repository_errors_map_to_the_right_http_error() {
        let not_found = ApiError::from(RepositoryError::NotFound {
            aggregate: "Order",
            id: "o-1".into(),
        });
        assert!(matches!(
            not_found,
            ApiError::NotFound {
                resource: "Order",
                ..
            }
        ));

        let conflict = ApiError::from(RepositoryError::Conflict {
            aggregate: "Outfit",
            id: "of-1".into(),
            expected_version: 2,
        });
        assert!(matches!(conflict, ApiError::Conflict(_)));

        let invariant = ApiError::from(RepositoryError::InvariantViolation {
            aggregate: "PlayerCollection",
            constraint: Some("copy_cap".into()),
            message: "over cap".into(),
        });
        assert!(matches!(invariant, ApiError::Unprocessable(_)));
    }
}
