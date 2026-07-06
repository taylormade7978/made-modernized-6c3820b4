//! The trusted-gateway identity extractor.
//!
//! Authentication is **not** this service's job: the Kong gateway and its OPA
//! sidecar verify the caller's JWT, evaluate policy, and then inject the
//! resolved principal into the upstream request as plain headers. By the time a
//! request reaches an actix handler the auth decision is already made, so this
//! extractor only *reads* those headers — it never parses a token, checks a
//! signature, or re-derives a claim. That is the whole point of the sidecar
//! topology: keep crypto/authz in one audited place, not smeared across every
//! service.
//!
//! Three headers are consumed:
//!
//! * `X-Tenant-Id` — the tenant the caller is acting within (required).
//! * `X-Player-Id` — the authenticated principal, the player subject (required).
//! * `X-Roles` — the caller's comma-separated roles (optional); an internal
//!   service account carries the `service` role, which gates the privileged,
//!   server-authoritative grant/reveal operations and bypasses per-player
//!   ownership checks (fulfillment acting on a player's behalf).
//!
//! A missing *required* header is an [`ApiError::Unauthenticated`] (401): in a
//! correctly-wired deployment the gateway always sets them, so their absence
//! means the request bypassed the gateway (or it is misconfigured) — either way
//! the handler must not proceed as some ambiguous identity.

use std::future::{ready, Ready};

use actix_web::{dev::Payload, FromRequest, HttpRequest};

use super::envelope::ApiError;

/// Header the gateway sets to the caller's tenant.
const TENANT_HEADER: &str = "X-Tenant-Id";
/// Header the gateway sets to the authenticated player subject.
const PLAYER_HEADER: &str = "X-Player-Id";
/// Header the gateway sets to the caller's comma-separated roles. Absent for an
/// ordinary player; internal service accounts carry the `service` role.
const ROLES_HEADER: &str = "X-Roles";
/// The role name that marks an internal, server-authoritative caller (e.g. the
/// fulfillment service) allowed to perform privileged grants.
const SERVICE_ROLE: &str = "service";

/// The caller's trusted identity, lifted from gateway-set headers.
///
/// Handlers take this as an argument to scope work to the right tenant/player.
/// Crucially, *creates* draw their owner from [`Identity::player_id`] — never
/// from the request body — so a client cannot forge ownership of a resource for
/// another player.
#[derive(Debug, Clone)]
pub struct Identity {
    /// The tenant the caller is acting within (`X-Tenant-Id`).
    pub tenant_id: String,
    /// The authenticated player subject (`X-Player-Id`).
    pub player_id: String,
    /// Roles the gateway attached to the caller (`X-Roles`). Empty for an
    /// ordinary player.
    pub roles: Vec<String>,
}

impl Identity {
    /// Emit a structured audit line for a state-changing `action`.
    ///
    /// Authentication and authorization are enforced upstream by the gateway;
    /// this only records, after the fact, *which player* acted in *which
    /// tenant* — the traceability a service behind a trusted gateway still owes
    /// even though it does not make the auth decision itself.
    pub fn audit(&self, action: &str) {
        println!(
            "audit tenant={} player={} action={action}",
            self.tenant_id, self.player_id
        );
    }

    /// Whether the caller is an internal service account (carries the `service`
    /// role) rather than an ordinary player.
    pub fn is_service(&self) -> bool {
        self.roles.iter().any(|r| r == SERVICE_ROLE)
    }

    /// Guard a privileged, server-authoritative operation: `Ok` only for a
    /// service-role caller, otherwise a 403. This is what keeps a player from
    /// invoking a grant/reveal whose *contents* are supplied by the caller —
    /// those must originate from an internal service (fulfillment), never a
    /// player's browser.
    pub fn require_service(&self, action: &str) -> Result<(), ApiError> {
        if self.is_service() {
            Ok(())
        } else {
            Err(ApiError::Forbidden(format!(
                "'{action}' requires an internal service role"
            )))
        }
    }

    /// Guard object-level ownership: `Ok` when `owner` is this caller (or the
    /// caller is a service account acting on a player's behalf), otherwise a
    /// [`ApiError::NotFound`] for `resource`/`id`. `NotFound` (not `Forbidden`)
    /// is deliberate — a mismatch must not reveal that the resource exists under
    /// another player.
    pub fn require_owner(
        &self,
        owner: &str,
        resource: &'static str,
        id: &str,
    ) -> Result<(), ApiError> {
        if self.is_service() || owner == self.player_id {
            Ok(())
        } else {
            Err(ApiError::NotFound {
                resource,
                id: id.to_string(),
            })
        }
    }
}

/// Read a required header as an owned `String`, or `None` if it is absent or not
/// valid UTF-8.
fn header(req: &HttpRequest, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
}

impl FromRequest for Identity {
    type Error = ApiError;
    type Future = Ready<Result<Self, ApiError>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        let tenant_id = header(req, TENANT_HEADER);
        let player_id = header(req, PLAYER_HEADER);
        // Roles are optional; split the comma-separated header and drop blanks.
        let roles = header(req, ROLES_HEADER)
            .map(|raw| {
                raw.split(',')
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        ready(match (tenant_id, player_id) {
            (Some(tenant_id), Some(player_id)) => Ok(Identity {
                tenant_id,
                player_id,
                roles,
            }),
            _ => Err(ApiError::Unauthenticated(format!(
                "missing trusted gateway identity headers ({TENANT_HEADER} / {PLAYER_HEADER})"
            ))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(id: &str) -> Identity {
        Identity {
            tenant_id: "t-1".into(),
            player_id: id.into(),
            roles: vec![],
        }
    }

    fn service() -> Identity {
        Identity {
            tenant_id: "t-1".into(),
            player_id: "svc".into(),
            roles: vec![SERVICE_ROLE.into()],
        }
    }

    #[test]
    fn owner_may_act_on_own_resource() {
        assert!(player("p-1").require_owner("p-1", "Order", "o-1").is_ok());
    }

    #[test]
    fn non_owner_gets_not_found_not_forbidden() {
        // NotFound is deliberate: a mismatch must not reveal the resource exists.
        let err = player("p-1")
            .require_owner("p-2", "Order", "o-1")
            .unwrap_err();
        assert!(matches!(
            err,
            ApiError::NotFound {
                resource: "Order",
                ..
            }
        ));
    }

    #[test]
    fn service_role_bypasses_ownership_and_passes_service_gate() {
        let svc = service();
        assert!(svc.is_service());
        // A service acts on any player's resource (fulfillment on their behalf).
        assert!(svc.require_owner("p-anyone", "Order", "o-1").is_ok());
        assert!(svc.require_service("collection.grant").is_ok());
    }

    #[test]
    fn player_is_forbidden_from_a_service_only_operation() {
        let err = player("p-1")
            .require_service("collection.grant")
            .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(_)));
    }
}
