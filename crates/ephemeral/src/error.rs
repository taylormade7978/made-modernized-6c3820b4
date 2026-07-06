//! The error type raised by the Redis ephemeral-state adapters.
//!
//! Every fallible operation in this crate returns [`EphemeralError`]. It keeps
//! the underlying `redis`/`serde_json` failures typed (so callers can match on
//! them) while wrapping the pool lifecycle errors — which are generic over the
//! pool's connection type — as messages so this crate's public surface does not
//! leak `deadpool`'s type parameters.

use std::error::Error;
use std::fmt;

use deadpool_redis::redis::RedisError;
use deadpool_redis::PoolError;

/// A failure raised by one of the Redis ephemeral-state adapters.
#[derive(Debug)]
pub enum EphemeralError {
    /// The [`RedisConfig`](crate::RedisConfig) could not be read from the
    /// environment (e.g. a numeric setting was non-numeric).
    Config(String),
    /// The connection pool could not be built, or Redis was unreachable within
    /// the configured connect timeout — the fail-fast path.
    Connection(String),
    /// A Redis command failed on the wire.
    Redis(RedisError),
    /// A value could not be (de)serialized to/from its Redis representation.
    Serialization(serde_json::Error),
}

impl fmt::Display for EphemeralError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EphemeralError::Config(reason) => write!(f, "redis config error: {reason}"),
            EphemeralError::Connection(reason) => write!(f, "redis connection error: {reason}"),
            EphemeralError::Redis(err) => write!(f, "redis command error: {err}"),
            EphemeralError::Serialization(err) => write!(f, "redis serialization error: {err}"),
        }
    }
}

impl Error for EphemeralError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            EphemeralError::Redis(err) => Some(err),
            EphemeralError::Serialization(err) => Some(err),
            EphemeralError::Config(_) | EphemeralError::Connection(_) => None,
        }
    }
}

impl From<RedisError> for EphemeralError {
    fn from(err: RedisError) -> Self {
        EphemeralError::Redis(err)
    }
}

impl From<serde_json::Error> for EphemeralError {
    fn from(err: serde_json::Error) -> Self {
        EphemeralError::Serialization(err)
    }
}

impl From<PoolError> for EphemeralError {
    fn from(err: PoolError) -> Self {
        EphemeralError::Connection(format!("could not acquire connection: {err}"))
    }
}

/// Convenience alias for results returned by this crate's adapters.
pub type Result<T> = std::result::Result<T, EphemeralError>;
