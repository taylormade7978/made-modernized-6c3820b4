//! Redis ephemeral-state adapter for MADE.
//!
//! This crate is an *outer adapter* of the hexagon, sibling to
//! [`persistence`](../persistence) (PostgreSQL, the durable store). Where
//! PostgreSQL is the record of truth, Redis holds the *ephemeral* state a live
//! match needs and can safely lose on restart: the authoritative match snapshot,
//! session/presence heartbeats, the matchmaking queues the dual-axis matchmaker
//! consumes, and the pub/sub fan-out of match events. It runs on the **shared
//! VForce360 Redis**, so every key is namespaced to MADE to avoid collision with
//! neighbouring tenants.
//!
//! The domain kernel (`shared`) and the bounded contexts (`domain`,
//! `game-session`) stay framework-free — nothing in them depends on `redis` —
//! exactly as they stay free of `sqlx`.
//!
//! # Shape
//!
//! [`connect`] reads a [`RedisConfig`] (from the environment via
//! [`RedisConfig::from_env`], or built directly), opens a pooled connection, and
//! *fails fast* if Redis is unreachable. The returned [`RedisHandle`] hands out
//! four capability adapters, each mapping one acceptance criterion:
//!
//! | Adapter | Capability |
//! |---------|------------|
//! | [`MatchStateStore`] | live match snapshots, written/read with a configurable TTL |
//! | [`SessionStore`] | session / presence heartbeat keys |
//! | [`MatchmakingQueue`] | enqueue / dequeue + dual-axis (MMR + secondary) candidate lookup |
//! | [`MatchEventChannel`] | publish / subscribe match events |
//!
//! # Example
//!
//! ```no_run
//! # async fn run() -> ephemeral::Result<()> {
//! use std::time::Duration;
//! use ephemeral::{connect, RedisConfig, Candidate, MatchEvent};
//!
//! // Configurable via env (MADE_REDIS_URL, MADE_REDIS_NAMESPACE, …); fails fast
//! // here if Redis is unreachable.
//! let handle = connect(&RedisConfig::from_env()?).await?;
//!
//! // Live match state with a configurable TTL.
//! let matches = handle.match_state();
//! matches
//!     .write_snapshot("m-1", b"snapshot-bytes", Some(Duration::from_secs(300)))
//!     .await?;
//! let snapshot = matches.read_snapshot("m-1").await?;
//!
//! // Dual-axis matchmaking queue.
//! let queue = handle.matchmaking();
//! queue.enqueue("ranked", &Candidate::new("p-1", 1500.0, 12)).await?;
//! let target = Candidate::new("p-2", 1490.0, 11);
//! let candidates = queue.find_candidates("ranked", &target, 150.0, 5, 10).await?;
//!
//! // Match-event pub/sub.
//! let events = handle.events();
//! events
//!     .publish(&MatchEvent::new("m-1", "match.started", serde_json::json!({})))
//!     .await?;
//! # Ok(())
//! # }
//! ```

mod config;
mod error;
mod keys;
mod match_state;
mod matchmaking;
mod pool;
mod pubsub;
mod session;

pub use config::{
    RedisConfig, DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_NAMESPACE, DEFAULT_POOL_MAX_SIZE,
    DEFAULT_TTL_SECS, DEFAULT_URL,
};
pub use error::{EphemeralError, Result};
pub use keys::Keys;
pub use match_state::MatchStateStore;
pub use matchmaking::{within_secondary_band, Candidate, MatchmakingQueue};
pub use pool::{connect, RedisHandle};
pub use pubsub::{MatchEvent, MatchEventChannel, MatchEventSubscription};
pub use session::SessionStore;
