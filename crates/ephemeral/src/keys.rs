//! Namespaced key construction.
//!
//! Every key and pub/sub channel this crate touches is built through [`Keys`],
//! which prefixes the configured namespace onto a stable, colon-delimited key
//! shape. Because MADE shares the VForce360 Redis with other tenants, a single
//! source of truth for key layout is what keeps two projects from clobbering
//! each other's data (acceptance criterion: *keys are namespaced to the project
//! to avoid collision on shared Redis*).
//!
//! Key shapes (for namespace `made`):
//!
//! | Concern | Key / channel | Redis type |
//! |---------|---------------|------------|
//! | live match snapshot | `made:match:state:{match_id}` | string (TTL'd) |
//! | session / presence | `made:session:{session_id}` | string (TTL'd) |
//! | matchmaking queue (primary MMR axis) | `made:mmq:{queue}:mmr` | sorted set |
//! | matchmaking queue (secondary axis) | `made:mmq:{queue}:secondary` | hash |
//! | match-event channel | `made:events:match:{match_id}` | pub/sub channel |

/// Builds namespaced Redis keys and channel names from a project namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keys {
    namespace: String,
}

impl Keys {
    /// A key builder for the given namespace (e.g. `"made"`).
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
        }
    }

    /// The namespace every key produced here is prefixed with.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The string key holding a live match's serialized snapshot.
    pub fn match_state(&self, match_id: &str) -> String {
        format!("{}:match:state:{match_id}", self.namespace)
    }

    /// The string key holding a session's presence marker.
    pub fn session(&self, session_id: &str) -> String {
        format!("{}:session:{session_id}", self.namespace)
    }

    /// The sorted-set key for a matchmaking queue's *primary* (MMR) axis — the
    /// set score is the candidate's MMR, the member is the candidate id.
    pub fn queue_mmr(&self, queue_id: &str) -> String {
        format!("{}:mmq:{queue_id}:mmr", self.namespace)
    }

    /// The hash key for a matchmaking queue's *secondary* axis — one field per
    /// enqueued member holding its secondary-axis value (e.g. level).
    pub fn queue_secondary(&self, queue_id: &str) -> String {
        format!("{}:mmq:{queue_id}:secondary", self.namespace)
    }

    /// The pub/sub channel match events for `match_id` are published to.
    pub fn match_events(&self, match_id: &str) -> String {
        format!("{}:events:match:{match_id}", self.namespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_is_prefixed_with_the_namespace() {
        let keys = Keys::new("made");
        for key in [
            keys.match_state("m1"),
            keys.session("s1"),
            keys.queue_mmr("ranked"),
            keys.queue_secondary("ranked"),
            keys.match_events("m1"),
        ] {
            assert!(
                key.starts_with("made:"),
                "key '{key}' is not namespaced under 'made:'"
            );
        }
    }

    #[test]
    fn distinct_namespaces_never_collide() {
        let made = Keys::new("made");
        let other = Keys::new("othergame");
        // The same logical id under two namespaces must map to distinct keys —
        // that is the whole point of namespacing on a shared Redis.
        assert_ne!(made.match_state("m1"), other.match_state("m1"));
        assert_ne!(made.queue_mmr("ranked"), other.queue_mmr("ranked"));
    }

    #[test]
    fn queue_axes_are_distinct_keys() {
        let keys = Keys::new("made");
        // The primary (sorted set) and secondary (hash) axes of one queue must
        // be separate keys — they are different Redis types.
        assert_ne!(keys.queue_mmr("ranked"), keys.queue_secondary("ranked"));
    }
}
