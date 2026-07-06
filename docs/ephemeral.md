# Ephemeral state: Redis adapters

Redis is the **ephemeral** store for MADE — the transient state a live match
needs and can lose on restart, in contrast to the non-substitutable PostgreSQL
record of truth (`docs/persistence.md`). It runs on the **shared VForce360
Redis**, so isolation from neighbouring tenants is a first-class concern.

`crates/ephemeral` is the adapter. Like `crates/persistence` it is an *outer
adapter* of the hexagon: the domain kernel (`shared`) and the bounded contexts
(`domain`, `game-session`) never depend on `redis`.

## Capabilities

`connect(&RedisConfig)` returns a `RedisHandle` that hands out four capability
adapters, each mapping directly onto a story acceptance criterion:

| Adapter | Redis structure(s) | Capability |
|---------|--------------------|------------|
| `MatchStateStore` | string with `SET … EX` | live match snapshots written/read within the match lifecycle with a configurable TTL |
| `SessionStore` | string with `SET … EX` | session/presence heartbeat keys that self-expire when heartbeats stop |
| `MatchmakingQueue` | sorted set (`…:mmr`) + hash (`…:secondary`) | enqueue / dequeue and dual-axis (MMR + secondary) candidate lookup |
| `MatchEventChannel` | pub/sub channel | publish match events / subscribe on a dedicated connection |

## Key namespacing (shared Redis)

Every key and channel is built through `Keys`, which prefixes the configured
namespace (default `made`) onto a stable, colon-delimited shape. This is what
keeps two tenants on the shared Redis from clobbering each other's data.

| Concern | Key / channel (namespace `made`) | Type |
|---------|----------------------------------|------|
| live match snapshot | `made:match:state:{match_id}` | string (TTL'd) |
| session / presence | `made:session:{session_id}` | string (TTL'd) |
| matchmaking — primary MMR axis | `made:mmq:{queue}:mmr` | sorted set |
| matchmaking — secondary axis | `made:mmq:{queue}:secondary` | hash |
| match-event channel | `made:events:match:{match_id}` | pub/sub channel |

## Dual-axis matchmaking

The matchmaker pairs on two axes — a primary MMR axis and a secondary axis (e.g.
level), mirroring the ±Rating / ±Level search bands the `MatchmakingTicket`
aggregate enforces. Redis has no native 2-D index, so a queue is two structures:
a **sorted set** scored by MMR (the axis Redis can range-scan) plus a **hash** of
each member's secondary value. `find_candidates` range-scans the MMR band with
`ZRANGEBYSCORE`, then filters the returned members against the secondary band in
the adapter (the pure, unit-tested `within_secondary_band` predicate), excluding
the target itself.

## Configuration (fail-fast pool)

`RedisConfig::from_env()` reads every tunable a deployment needs; a malformed
numeric setting is a hard error rather than a silent fallback.

| Variable | Meaning | Default |
|----------|---------|---------|
| `MADE_REDIS_URL` (or `REDIS_URL`) | endpoint URL | `redis://127.0.0.1:6379` |
| `MADE_REDIS_NAMESPACE` | key namespace | `made` |
| `MADE_REDIS_POOL_MAX_SIZE` | connection-pool size | `16` |
| `MADE_REDIS_CONNECT_TIMEOUT_MS` | connect timeout (ms) | `2000` |
| `MADE_REDIS_DEFAULT_TTL_SECS` | default snapshot TTL (s) | `3600` |

`connect` builds a `deadpool-redis` pool sized from `pool_max_size`, then
*actively probes* with a `PING` bounded by `connect_timeout`. deadpool creates
connections lazily, so without that probe an unreachable Redis would only surface
on the first real command; probing up front makes `connect` **fail fast**.

## Testing

Unit tests cover the pure, server-free logic — config parsing/defaults, key
namespacing, the dual-axis secondary-band predicate, and match-event JSON
round-tripping — and run in the ordinary `build-and-test` CI job.

The live-Redis behaviour (TTL round-trips, queue enqueue/dequeue/lookup, and
pub/sub delivery) lives in `tests/redis_integration.rs`. That job's CI runner has
no Redis service, so the tests **self-skip** unless a reachable Redis is named:

```sh
docker run --rm -p 6379:6379 redis:7
MADE_REDIS_URL=redis://127.0.0.1:6379 \
  cargo test -p ephemeral --test redis_integration
```

Each integration test uses a unique namespace so concurrent runs never collide
on the shared instance, and cleans up the keys it creates. (The fail-fast
connect test needs no server — it points at a dead port and asserts the timeout
fires.)
