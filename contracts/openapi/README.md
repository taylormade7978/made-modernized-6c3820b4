# MADE `/v1` API contract

`made-v1.yaml` is the **single source of truth** for the MADE `/v1` HTTP contract.
It exists to close [#106](https://github.com/edgentx/made-modernized-6c3820b4/issues/106)
(the PWA client and the Rust server were built to divergent `/v1` shapes, so the
data screens 404'd) and to realise the [#109](https://github.com/edgentx/made-modernized-6c3820b4/pull/109)
architecture decision: **one canonical Rust engine** serves the app — no GraphQL
surface, no separate Node data service.

## Decisions captured here

- **REST-only.** Reads and writes are served by `crates/server` over REST. The
  transitional `services/data` GraphQL shim (introduced in #107/#115) is retired
  once every client resource is migrated onto these routes.
- **`camelCase`, client-shaped.** DTO field names follow `web/src/api/types.ts`,
  which already mirrors the Rust domain aggregates. Server DTOs conform *to this
  document* (`#[serde(rename_all = "camelCase")]`).
- **Envelope.** Every 2xx body is `{ "data": <payload> }`; errors are
  `{ "error": { code, message, details? } }` (see `crates/server/src/http/envelope.rs`).
  The schemas under each path describe the **payload** carried in `data`.

## Migration plan (keeps `main` green + the app working at every step)

Each resource migrates in its own PR: add/adjust the Rust route + DTO (+ repo
list method where needed), then flip that resource's client calls from GraphQL to
REST. Un-migrated resources keep using the still-deployed `services/data` shim, so
the app never breaks mid-migration.

1. **Contract** — this file. *(supersedes #106; no behaviour change)*
2. **catalog** — `GET /catalog/cards`, `GET /catalog/expansions` (+ enrich DTOs)
3. **collection** — `GET /collection/{playerId}`, `PUT …/decks/{deckId}`
4. **leaderboard** — `GET /leaderboard` (default season)
5. **shop** — `GET /shop/items`, `POST`/`GET /shop/orders`
6. **story** — `GET /story/{playerId}/missions`, `POST …/attempts`
   (seeded Neon Heist campaign served from Rust, mirroring #115's shapes)
7. **retire** — delete `services/data`, `web/src/api/graphql.ts`, and `graphqlUrl`;
   close #107/#115 as superseded and #106 as fixed.

## Validating

```bash
# structural lint (any OpenAPI 3.1 linter)
npx @redocly/cli lint contracts/openapi/made-v1.yaml
```
