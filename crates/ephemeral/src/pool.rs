//! The connection pool and the fail-fast connect path.
//!
//! [`connect`] builds a [`deadpool_redis`] connection pool sized from the
//! [`RedisConfig`], then *actively probes* Redis with a `PING` bounded by the
//! configured connect timeout. deadpool creates connections lazily, so without
//! that probe an unreachable Redis would only surface on the first real command;
//! probing up front makes [`connect`] fail fast (acceptance criterion:
//! *connection pool is configurable via environment/config and fails fast on
//! unreachable Redis*).
//!
//! The returned [`RedisHandle`] is the entrypoint: it hands out the three
//! capability adapters — [`MatchStateStore`], [`SessionStore`],
//! [`MatchmakingQueue`], and [`MatchEventChannel`] — each pre-wired with the
//! shared pool and the namespaced [`Keys`].

use std::time::Duration;

use deadpool_redis::redis::{cmd, Client};
use deadpool_redis::{Config, Pool, PoolConfig, Runtime};

use crate::error::{EphemeralError, Result};
use crate::keys::Keys;
use crate::match_state::MatchStateStore;
use crate::matchmaking::MatchmakingQueue;
use crate::pubsub::MatchEventChannel;
use crate::session::SessionStore;
use crate::RedisConfig;

/// A live, pooled connection to the shared Redis plus the namespaced key
/// builder. Clone-cheap (the pool and pub/sub client are `Arc`-backed) and the
/// single source of the capability adapters.
#[derive(Clone)]
pub struct RedisHandle {
    pool: Pool,
    client: Client,
    keys: Keys,
    default_ttl: Duration,
}

impl RedisHandle {
    /// The namespaced key builder every adapter shares.
    pub fn keys(&self) -> &Keys {
        &self.keys
    }

    /// The underlying command connection pool.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// The live match-snapshot store (with the config's default TTL).
    pub fn match_state(&self) -> MatchStateStore {
        MatchStateStore::new(self.pool.clone(), self.keys.clone(), self.default_ttl)
    }

    /// The session/presence store.
    pub fn sessions(&self) -> SessionStore {
        SessionStore::new(self.pool.clone(), self.keys.clone(), self.default_ttl)
    }

    /// The dual-axis matchmaking queue.
    pub fn matchmaking(&self) -> MatchmakingQueue {
        MatchmakingQueue::new(self.pool.clone(), self.keys.clone())
    }

    /// The match-event pub/sub channel. Publishing uses the pool; subscribing
    /// takes a dedicated connection from the pub/sub `client`.
    pub fn events(&self) -> MatchEventChannel {
        MatchEventChannel::new(self.pool.clone(), self.client.clone(), self.keys.clone())
    }
}

/// Build the pool from `config` and fail fast if Redis is unreachable.
///
/// Returns [`EphemeralError::Connection`] if the pool cannot be constructed, the
/// URL is malformed, or the `PING` probe does not complete within
/// [`RedisConfig::connect_timeout`].
pub async fn connect(config: &RedisConfig) -> Result<RedisHandle> {
    // Reject an unsafe namespace before opening anything — covers configs built
    // with the `with_*` setters, not just those from the environment.
    config.validate()?;

    let mut cfg = Config::from_url(&config.url);
    // Size the pool from config (acceptance criterion: pool configurable).
    cfg.pool = Some(PoolConfig::new(config.pool_max_size));
    let pool = cfg
        .create_pool(Some(Runtime::Tokio1))
        .map_err(|e| EphemeralError::Connection(format!("could not build pool: {e}")))?;

    // A dedicated client for pub/sub subscribers (a subscribing connection is
    // taken out of multiplexing, so it cannot come from the command pool).
    let client = Client::open(config.url.as_str())
        .map_err(|e| EphemeralError::Connection(format!("invalid redis url: {e}")))?;

    // Fail-fast reachability probe, bounded by the configured connect timeout.
    probe(&pool, config.connect_timeout).await?;

    Ok(RedisHandle {
        pool,
        client,
        keys: Keys::new(config.namespace.clone()),
        default_ttl: config.default_ttl,
    })
}

/// Actively `PING` Redis within `timeout`, turning both a timeout and a failed
/// ping into a [`EphemeralError::Connection`].
async fn probe(pool: &Pool, timeout: Duration) -> Result<()> {
    let ping = async {
        let mut conn = pool.get().await?;
        cmd("PING").query_async::<()>(&mut conn).await?;
        Ok::<(), EphemeralError>(())
    };

    match tokio::time::timeout(timeout, ping).await {
        Ok(result) => result,
        Err(_) => Err(EphemeralError::Connection(format!(
            "redis unreachable within {timeout:?}"
        ))),
    }
}
