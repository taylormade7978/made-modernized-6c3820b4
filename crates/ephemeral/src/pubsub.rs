//! Match-event pub/sub.
//!
//! Match events (a card played, heat raised, the match sealed) are fanned out to
//! every interested subscriber — spectators, the durable-replay writer, a
//! matchmaking observer — over a per-match Redis pub/sub channel. Publishers use
//! the shared command pool; each subscriber takes a *dedicated* connection from
//! the pub/sub client, because a subscribing connection leaves normal
//! request/response multiplexing.
//!
//! This satisfies the acceptance criterion: *pub/sub channel delivers match
//! events to subscribers in integration test* (see `tests/redis_integration.rs`).

use deadpool_redis::redis::aio::PubSub;
use deadpool_redis::redis::{AsyncCommands, Client};
use deadpool_redis::Pool;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::keys::Keys;

/// A match event as it travels on the pub/sub wire: which match it belongs to,
/// its stable type name (mirroring [`shared::DomainEvent::event_type`]), and an
/// opaque JSON payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchEvent {
    /// The match this event belongs to (selects the channel).
    pub match_id: String,
    /// Stable event type name, e.g. `"card.played"`.
    pub event_type: String,
    /// Opaque event payload.
    pub payload: serde_json::Value,
}

impl MatchEvent {
    /// Build a match event.
    pub fn new(
        match_id: impl Into<String>,
        event_type: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            match_id: match_id.into(),
            event_type: event_type.into(),
            payload,
        }
    }
}

/// Publishes match events and opens subscriptions on the per-match channel.
#[derive(Clone)]
pub struct MatchEventChannel {
    pool: Pool,
    client: Client,
    keys: Keys,
}

impl MatchEventChannel {
    pub(crate) fn new(pool: Pool, client: Client, keys: Keys) -> Self {
        Self { pool, client, keys }
    }

    /// Publish `event` to its match's channel. Returns the number of subscribers
    /// that received it (Redis `PUBLISH`'s return value).
    pub async fn publish(&self, event: &MatchEvent) -> Result<u64> {
        let channel = self.keys.match_events(&event.match_id);
        let payload = serde_json::to_vec(event)?;
        let mut conn = self.pool.get().await?;
        let receivers: u64 = conn.publish(channel, payload).await?;
        Ok(receivers)
    }

    /// Open a subscription to `match_id`'s event channel on a dedicated
    /// connection. Drive it with [`MatchEventSubscription::next_event`].
    pub async fn subscribe(&self, match_id: &str) -> Result<MatchEventSubscription> {
        let channel = self.keys.match_events(match_id);
        let mut pubsub = self.client.get_async_pubsub().await?;
        pubsub.subscribe(&channel).await?;
        Ok(MatchEventSubscription { pubsub })
    }
}

/// A live subscription to one match's event channel.
pub struct MatchEventSubscription {
    pubsub: PubSub,
}

impl MatchEventSubscription {
    /// Await the next match event on the channel, or `None` if the connection
    /// closed. A malformed (non-[`MatchEvent`]) message is a
    /// [`crate::EphemeralError::Serialization`].
    pub async fn next_event(&mut self) -> Result<Option<MatchEvent>> {
        let mut stream = self.pubsub.on_message();
        match stream.next().await {
            None => Ok(None),
            Some(msg) => {
                let event: MatchEvent = serde_json::from_slice(msg.get_payload_bytes())?;
                Ok(Some(event))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_event_round_trips_through_json() {
        let event = MatchEvent::new(
            "m-1",
            "card.played",
            serde_json::json!({ "card": "Slugger", "target": "p-2" }),
        );
        let bytes = serde_json::to_vec(&event).unwrap();
        let decoded: MatchEvent = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn match_event_uses_camel_case_wire_names() {
        let event = MatchEvent::new("m-1", "match.started", serde_json::json!({}));
        let text = serde_json::to_string(&event).unwrap();
        // The wire schema is camelCase (matchId/eventType), matching the queue
        // command payloads elsewhere in the codebase.
        assert!(text.contains("\"matchId\""), "got: {text}");
        assert!(text.contains("\"eventType\""), "got: {text}");
    }
}
