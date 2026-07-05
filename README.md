# made-modernized-6c3820b4

Made — Card Game modernization: taylormade7978/made → Rust/Actix + React PWA (VForce360 Track B)

## Architecture

A Cargo workspace following **DDD hexagonal** conventions. The domain core is
framework-free and WASM-safe; adapters live at the edges.

| Crate | Role | Notes |
|-------|------|-------|
| `crates/shared` | Domain kernel (hexagon core) | `Aggregate` contract with `execute(cmd)`, `AggregateRoot` base type (version + uncommitted events), `DomainEvent`, `DomainError::UnknownCommand`, `Repository<A>` port, and the `stub_aggregate!` macro. Zero external deps. |
| `crates/game-session` | GameSession aggregate / rules | Compiles natively **and** to WASM via `wasm-pack` (`--features wasm`) for shared server/client execution. |
| `crates/domain` | The other 7 bounded contexts | MatchReplay, CardDefinition, BossDefinition, ExpansionSet, MatchmakingTicket, RankedStanding, Season — each a stub aggregate + repository contract. |
| `crates/mocks` | In-memory repository adapters | One `InMemory*Repository` per aggregate, each implementing its domain repository contract. Home of the scaffold test suite. |
| `crates/server` | Authoritative WebSocket server | `actix-web` + `actix-ws` driving adapter, wiring aggregates to the mock repositories. |

### The Aggregate `Execute(cmd)` pattern

Every aggregate embeds `AggregateRoot` and implements `Aggregate::execute`, the
single write entrypoint. A command is a named message; an aggregate routes on
the name and, for anything it does not recognize, returns
`DomainError::UnknownCommand`. Stubs recognize *no* commands yet, so `execute`
currently always returns `UnknownCommand` — behavior is added by growing the
set of recognized command names.

## Commands

```sh
make build   # cargo build --workspace --all-targets
make test    # cargo test --workspace --no-run && cargo test --workspace
make wasm    # wasm-pack build crates/game-session -- --features wasm
make run     # run the actix-web + actix-ws game server on 127.0.0.1:8080
```
