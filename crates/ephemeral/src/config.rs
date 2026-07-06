//! Connection and namespacing configuration for the Redis ephemeral adapters.
//!
//! The shared VForce360 Redis is a *shared* instance, so every setting a
//! deployment needs to tune — where Redis lives, how large the pool is, how long
//! to wait before declaring it unreachable, the default snapshot TTL, and the
//! key namespace that isolates MADE's keys from its neighbours — is read from the
//! environment via [`RedisConfig::from_env`]. Tests and embedders can also build
//! a config directly with [`RedisConfig::new`] and the `with_*` setters.

use std::env;
use std::time::Duration;

use crate::error::{EphemeralError, Result};

/// The default Redis endpoint used when `MADE_REDIS_URL` / `REDIS_URL` are unset.
pub const DEFAULT_URL: &str = "redis://127.0.0.1:6379";

/// The default key namespace. Every key this crate writes is prefixed with it so
/// MADE cannot collide with another tenant on the shared Redis.
pub const DEFAULT_NAMESPACE: &str = "made";

/// The default connection-pool size.
pub const DEFAULT_POOL_MAX_SIZE: usize = 16;

/// The default connect timeout, in milliseconds — the fail-fast budget for the
/// initial reachability probe.
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 2_000;

/// The default TTL applied to live match snapshots when a caller does not pass
/// an explicit one, in seconds (one hour — comfortably longer than a match).
pub const DEFAULT_TTL_SECS: u64 = 3_600;

/// How the Redis adapters connect to, pool against, and namespace the shared
/// Redis instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisConfig {
    /// Redis endpoint URL, e.g. `redis://127.0.0.1:6379`.
    pub url: String,
    /// Key namespace prefixed onto every key (collision isolation on the shared
    /// Redis). See [`crate::Keys`].
    pub namespace: String,
    /// Maximum number of pooled connections.
    pub pool_max_size: usize,
    /// How long to wait for the initial reachability probe before failing fast.
    pub connect_timeout: Duration,
    /// Default TTL for live match snapshots when a caller passes none.
    pub default_ttl: Duration,
}

impl RedisConfig {
    /// A config pointing at `url` with all other fields at their defaults.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            namespace: DEFAULT_NAMESPACE.to_string(),
            pool_max_size: DEFAULT_POOL_MAX_SIZE,
            connect_timeout: Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS),
            default_ttl: Duration::from_secs(DEFAULT_TTL_SECS),
        }
    }

    /// Override the key namespace.
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Override the connection-pool size.
    pub fn with_pool_max_size(mut self, size: usize) -> Self {
        self.pool_max_size = size;
        self
    }

    /// Override the fail-fast connect timeout.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Override the default snapshot TTL.
    pub fn with_default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = ttl;
        self
    }

    /// Read the configuration from the environment.
    ///
    /// | Variable | Meaning | Default |
    /// |----------|---------|---------|
    /// | `MADE_REDIS_URL` (or `REDIS_URL`) | endpoint URL | [`DEFAULT_URL`] |
    /// | `MADE_REDIS_NAMESPACE` | key namespace | [`DEFAULT_NAMESPACE`] |
    /// | `MADE_REDIS_POOL_MAX_SIZE` | pool size | [`DEFAULT_POOL_MAX_SIZE`] |
    /// | `MADE_REDIS_CONNECT_TIMEOUT_MS` | connect timeout (ms) | [`DEFAULT_CONNECT_TIMEOUT_MS`] |
    /// | `MADE_REDIS_DEFAULT_TTL_SECS` | default snapshot TTL (s) | [`DEFAULT_TTL_SECS`] |
    ///
    /// A malformed numeric setting is a hard [`EphemeralError::Config`] rather
    /// than a silent fallback to the default, so a typo fails loudly.
    pub fn from_env() -> Result<Self> {
        let url = env::var("MADE_REDIS_URL")
            .or_else(|_| env::var("REDIS_URL"))
            .unwrap_or_else(|_| DEFAULT_URL.to_string());

        let namespace =
            env::var("MADE_REDIS_NAMESPACE").unwrap_or_else(|_| DEFAULT_NAMESPACE.to_string());

        let pool_max_size = parse_env("MADE_REDIS_POOL_MAX_SIZE", DEFAULT_POOL_MAX_SIZE)?;
        let connect_timeout_ms =
            parse_env("MADE_REDIS_CONNECT_TIMEOUT_MS", DEFAULT_CONNECT_TIMEOUT_MS)?;
        let default_ttl_secs = parse_env("MADE_REDIS_DEFAULT_TTL_SECS", DEFAULT_TTL_SECS)?;

        let config = Self {
            url,
            namespace,
            pool_max_size,
            connect_timeout: Duration::from_millis(connect_timeout_ms),
            default_ttl: Duration::from_secs(default_ttl_secs),
        };
        config.validate()?;
        Ok(config)
    }

    /// Validate the settings that carry cross-tenant safety weight.
    ///
    /// The namespace is the collision barrier on the shared Redis, so a
    /// malformed one is as fatal as a malformed numeric setting: it must be
    /// non-empty and free of `:` (the key separator), whitespace, and control
    /// characters — anything that could let one tenant's keys reach into
    /// another's keyspace. Called by [`from_env`](Self::from_env) and again by
    /// [`connect`](crate::connect), so a config built with the `with_*` setters
    /// is validated before it ever touches Redis.
    pub fn validate(&self) -> Result<()> {
        let ns = &self.namespace;
        if ns.is_empty() {
            return Err(EphemeralError::Config(
                "namespace must be non-empty to isolate MADE on the shared Redis".to_string(),
            ));
        }
        if ns
            .chars()
            .any(|c| c == ':' || c.is_whitespace() || c.is_control())
        {
            return Err(EphemeralError::Config(format!(
                "namespace '{ns}' must not contain ':' , whitespace, or control characters"
            )));
        }
        Ok(())
    }
}

/// Parse a numeric environment variable, defaulting when unset but erroring when
/// present-but-malformed.
fn parse_env<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(name) {
        Err(_) => Ok(default),
        Ok(raw) => raw.parse::<T>().map_err(|e| {
            EphemeralError::Config(format!("{name}='{raw}' is not a valid value: {e}"))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_documented_defaults() {
        let cfg = RedisConfig::new(DEFAULT_URL);
        assert_eq!(cfg.namespace, DEFAULT_NAMESPACE);
        assert_eq!(cfg.pool_max_size, DEFAULT_POOL_MAX_SIZE);
        assert_eq!(
            cfg.connect_timeout,
            Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS)
        );
        assert_eq!(cfg.default_ttl, Duration::from_secs(DEFAULT_TTL_SECS));
    }

    #[test]
    fn with_setters_override_fields() {
        let cfg = RedisConfig::new("redis://example:6379")
            .with_namespace("made-test")
            .with_pool_max_size(4)
            .with_connect_timeout(Duration::from_millis(500))
            .with_default_ttl(Duration::from_secs(120));
        assert_eq!(cfg.namespace, "made-test");
        assert_eq!(cfg.pool_max_size, 4);
        assert_eq!(cfg.connect_timeout, Duration::from_millis(500));
        assert_eq!(cfg.default_ttl, Duration::from_secs(120));
    }

    #[test]
    fn parse_env_defaults_when_unset() {
        // A name that is (essentially) certainly not set in the test environment.
        let got: usize = parse_env("MADE_REDIS_DEFINITELY_UNSET_XYZ", 7).unwrap();
        assert_eq!(got, 7);
    }

    #[test]
    fn validate_accepts_a_clean_namespace() {
        assert!(RedisConfig::new(DEFAULT_URL)
            .with_namespace("made-prod")
            .validate()
            .is_ok());
    }

    #[test]
    fn validate_rejects_empty_and_unsafe_namespaces() {
        // Empty: no isolation barrier at all.
        assert!(matches!(
            RedisConfig::new(DEFAULT_URL).with_namespace("").validate(),
            Err(EphemeralError::Config(_))
        ));
        // Contains the key separator ':' — could reach into another keyspace.
        assert!(matches!(
            RedisConfig::new(DEFAULT_URL)
                .with_namespace("made:evil")
                .validate(),
            Err(EphemeralError::Config(_))
        ));
        // Whitespace is likewise rejected.
        assert!(matches!(
            RedisConfig::new(DEFAULT_URL)
                .with_namespace("made prod")
                .validate(),
            Err(EphemeralError::Config(_))
        ));
    }
}
