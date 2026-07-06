//! The WebSocket wire protocol: the frames a client sends the authoritative
//! match channel, and the frames the server sends back.
//!
//! As with the REST envelope and the Redis pub/sub `MatchEvent`, this adapter
//! owns its wire schema — the framework-free rules crate never gains a wire
//! concern. Every frame is a JSON object discriminated by a `type` tag
//! (camelCase, matching the rest of the codebase's wire schemas).
//!
//! # Client → server ([`ClientMessage`])
//!
//! * `join` — subscribe to a match's authoritative delta stream. The server
//!   replies [`ServerMessage::Joined`] with the backlog of deltas so far, so a
//!   fresh *or reconnecting* client rebuilds its view from the authoritative
//!   record (acceptance criterion: *disconnect/reconnect resumes without
//!   corrupting the session*).
//! * `action` — submit a rules command. The client optimistically applies the
//!   same command locally (it runs the very same `game-session` rules crate via
//!   WASM); the server re-runs it authoritatively and either broadcasts the
//!   resulting deltas or returns a [`ServerMessage::Rejected`] correction.
//!
//! # Server → client ([`ServerMessage`])
//!
//! * `joined` — the current backlog after a join.
//! * `delta` — one authoritative state change, broadcast to *every* subscriber.
//! * `rejected` — the acting client's command was illegal; it carries the
//!   authoritative sequence the client must roll its optimistic state back to.
//! * `completed` — the match ended and its replay was sealed.
//! * `error` — a malformed frame or unknown match.

use serde::{Deserialize, Serialize};

/// A frame sent by a client to the match channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMessage {
    /// Subscribe to `matchId`'s authoritative stream as `playerId`.
    #[serde(rename_all = "camelCase")]
    Join {
        /// The match to join.
        match_id: String,
        /// The joining player's identity (must name one of the match's Outfits
        /// for its actions to be accepted).
        player_id: String,
    },
    /// Submit a rules command against the match. `command` is the aggregate
    /// command name (e.g. `"PlayCardCmd"`) and `payload` its JSON body — the
    /// exact `shared::Command` shape the `game-session` aggregate decodes, so a
    /// client reuses the same command it applied optimistically.
    #[serde(rename_all = "camelCase")]
    Action {
        /// The match the command targets.
        match_id: String,
        /// The acting player's identity.
        player_id: String,
        /// The aggregate command name to dispatch.
        command: String,
        /// The command's JSON payload (the aggregate deserializes it).
        #[serde(default)]
        payload: serde_json::Value,
    },
}

/// A frame the server sends a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMessage {
    /// Acknowledges a join and hands over the authoritative backlog: every delta
    /// recorded up to `sinceSeq`, so the client can (re)build its view.
    #[serde(rename_all = "camelCase")]
    Joined {
        /// The match joined.
        match_id: String,
        /// The highest sequence number present in `backlog` (0 if empty).
        since_seq: u64,
        /// Every authoritative delta so far, in order.
        backlog: Vec<StateDelta>,
    },
    /// One authoritative state change, broadcast to every subscriber of the
    /// match. Clients apply these in `seq` order as the source of truth.
    #[serde(rename_all = "camelCase")]
    Delta {
        /// The match the delta belongs to.
        match_id: String,
        /// The authoritative delta.
        delta: StateDelta,
    },
    /// The acting client's command was rejected server-side by the shared rules.
    /// The client must discard its optimistic prediction and resync its state to
    /// `authoritativeSeq` — the last delta the server actually applied.
    #[serde(rename_all = "camelCase")]
    Rejected {
        /// The match the command targeted.
        match_id: String,
        /// The rejected command name.
        command: String,
        /// The domain reason the rules gave for rejecting it.
        reason: String,
        /// The authoritative sequence to roll optimistic state back to.
        authoritative_seq: u64,
    },
    /// The match ended and its replay was sealed durably.
    #[serde(rename_all = "camelCase")]
    Completed {
        /// The completed match.
        match_id: String,
        /// The winning seat (`"A"` / `"B"`).
        winner: String,
        /// The sealed replay's identity, retrievable via the replay repository.
        replay_id: String,
        /// The replay's frame-stream checksum.
        checksum: String,
    },
    /// A protocol-level failure (malformed frame, unknown match).
    #[serde(rename_all = "camelCase")]
    Error {
        /// A human-readable description of what went wrong.
        message: String,
    },
}

/// One authoritative state change on the wire: the sequence number that orders
/// it in the match, the stable event type name (mirroring
/// [`shared::DomainEvent::event_type`]), and the event's serialized fields.
///
/// A delta is the *only* record of an authoritative change; a client's optimistic
/// state is only ever confirmed by receiving the matching delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateDelta {
    /// Contiguous, 1-based sequence number ordering the delta within the match.
    pub seq: u64,
    /// Stable event type, e.g. `"card.played"`.
    pub event_type: String,
    /// The event's fields as JSON.
    pub event: serde_json::Value,
}

/// The serialized live-match snapshot written to Redis after every authoritative
/// change, and read back to resume a match on reconnect.
///
/// It captures exactly what a resumed match needs that cannot be re-derived: the
/// ordered delta backlog (to re-serve a reconnecting client), the RNG cursor (so
/// the deterministic stream continues rather than restarts — see
/// [`super::rng::SeededRng::resume`]), the sequence high-water mark, and whether
/// the match has already completed. The snapshot is ephemeral: PostgreSQL holds
/// the sealed replay as the record of truth, so losing this on restart is safe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchSnapshot {
    /// The match this snapshot restores.
    pub match_id: String,
    /// The player who opened (hosts) the match.
    pub host_player_id: String,
    /// The seed the match's deterministic RNG was built from.
    pub rng_seed: u64,
    /// How many RNG values have been drawn — the cursor to resume the stream at.
    pub rng_draws: u64,
    /// The sequence high-water mark (== `deltas.last().seq`).
    pub sequence: u64,
    /// The ordered authoritative delta backlog.
    pub deltas: Vec<StateDelta>,
    /// Whether the match has already completed (its replay sealed).
    pub completed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_frame_round_trips_with_camel_case_tag() {
        let msg = ClientMessage::Join {
            match_id: "m-1".into(),
            player_id: "p-1".into(),
        };
        let text = serde_json::to_string(&msg).unwrap();
        assert!(text.contains("\"type\":\"join\""), "got {text}");
        assert!(text.contains("\"matchId\":\"m-1\""), "got {text}");
        assert_eq!(serde_json::from_str::<ClientMessage>(&text).unwrap(), msg);
    }

    #[test]
    fn action_payload_defaults_to_null_when_omitted() {
        let text = r#"{"type":"action","matchId":"m-1","playerId":"p-1","command":"EndTurnCmd"}"#;
        let msg: ClientMessage = serde_json::from_str(text).unwrap();
        match msg {
            ClientMessage::Action {
                command, payload, ..
            } => {
                assert_eq!(command, "EndTurnCmd");
                assert!(payload.is_null());
            }
            other => panic!("expected Action, got {other:?}"),
        }
    }

    #[test]
    fn server_delta_serializes_the_wire_schema() {
        let msg = ServerMessage::Delta {
            match_id: "m-1".into(),
            delta: StateDelta {
                seq: 3,
                event_type: "card.played".into(),
                event: serde_json::json!({ "player": "A" }),
            },
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "delta");
        assert_eq!(json["delta"]["seq"], 3);
        assert_eq!(json["delta"]["eventType"], "card.played");
    }

    #[test]
    fn snapshot_round_trips() {
        let snap = MatchSnapshot {
            match_id: "m-1".into(),
            host_player_id: "p-1".into(),
            rng_seed: 42,
            rng_draws: 2,
            sequence: 1,
            deltas: vec![StateDelta {
                seq: 1,
                event_type: "match.started".into(),
                event: serde_json::json!({}),
            }],
            completed: false,
        };
        let bytes = serde_json::to_vec(&snap).unwrap();
        assert_eq!(
            serde_json::from_slice::<MatchSnapshot>(&bytes).unwrap(),
            snap
        );
    }
}
