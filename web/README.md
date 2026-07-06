# MADE PWA (`web/`)

Mobile-first **React + TypeScript** PWA client for the MADE card game
(VForce360 Track B). Vite build, client-side routing, a service worker + web
app manifest for offline / OTA bundles, and a **shell-agnostic** bundle that
runs standalone in a browser and, unchanged, inside a Capacitor native
container.

## Commands

```sh
npm install
npm run dev        # Vite dev server
npm run build      # prebuild (stage-wasm) → tsc --noEmit → vite build → dist/
npm run stage:wasm # copy the rules-WASM pkg into public/vendor/ (run after `make wasm`)
npm run preview    # serve the production build locally
npm run icons      # regenerate PWA raster icons (public/icons/*.png)
```

## Rules-WASM in the bundle

The shared rules crate (`crates/game-session`) compiles to WASM and acts as the
optimistic layer's authoritative command name-gate (`src/match/wasm.ts`). The
loader imports the bare specifier `game-session` with `@vite-ignore`, so Vite
never bundles it and the gate degrades to disabled when the artifact is absent
(a plain `npm run build` without the Rust/wasm toolchain still succeeds).

To include the gate in a build:

```sh
make wasm            # wasm-pack build crates/game-session --target web → pkg/
npm run build        # `prebuild` stages pkg/ → public/vendor/game-session/, then vite build
```

`public/vendor/game-session/{game_session.js,game_session_bg.wasm}` ship into
`dist/` verbatim; `index.html`'s import map resolves the specifier to them and
`game_session.js` fetches its sibling `.wasm` relative to `import.meta.url`. The
service-worker precache glob includes `.wasm`, so the gate is available offline.

## Container image & deploy pipeline

`Dockerfile` is a three-stage production build (context = **repo root**, since
the wasm crate lives outside `web/`):

1. **wasm** — `wasm-pack` compiles the rules crate to WASM.
2. **web** — `npm ci` + `npm run build`, consuming the WASM pkg from stage 1.
3. **runtime** — hardened, **rootless** NGINX (`nginx-unprivileged`, uid 101,
   port 8080) serving the immutable static bundle with security headers, the
   correct `application/wasm` MIME, a long-cache/immutable asset policy, an
   always-revalidated app shell + service worker, an SPA fallback, and a
   `/healthz` liveness endpoint (wired to a Docker `HEALTHCHECK`).

```sh
make web-image   # docker build -f web/Dockerfile -t made-pwa:local .
make web-run     # run it on http://localhost:8080 (health check at /healthz)
```

CI (`.github/workflows/ci.yml → web-image`) builds the image on every run
(validating the Dockerfile) and, on a push to `main`:

- tags + pushes the image to the **deploy-configured registry** (`vars.WEB_REGISTRY`
  / `vars.WEB_IMAGE_NAME`, default GHCR), tagged with the commit sha and the
  version `<pkg-version>+<short-sha>` (also stamped into an image label); and
- publishes a **versioned OTA bundle** to MinIO via `scripts/publish-ota.sh`
  under `<app>/bundles/<version>/`, then moves a `latest.json` pointer last so a
  half-uploaded bundle is never live and a rollback is a pointer move. The
  `autoUpdate` service worker refreshes browser clients; native Capacitor shells
  pull the same bundle. Target host: `made.vforce360.ai`.

The `dist` Dockerfile stage exposes the built bundle for CI to export
(`--target dist -o type=local`) so the OTA bytes are exactly those baked into
the runtime image.

## Routing

Hash-based client routing (`createHashRouter`) — no server rewrite needed, so
the same `dist/` resolves over `http(s)://`, `file://`, and `capacitor://`.

Core routes (always present): `/match`, `/collection`, `/shop`,
`/leaderboard`, `/story`. Each is a placeholder filled in by a later story.

## Capability flags (build-time gating)

The token / marketplace / wallet flows are gated so native app-store shells can
disable them (store-policy compliance). Flags are parsed from `VITE_CAP_*` env
vars in `vite.config.ts` and injected as literal `define` globals, so a disabled
flow's route **and its JS chunk are eliminated** from the build — not merely
hidden. Unset ⇒ enabled (full open-web build). See `.env.example`.

```sh
# Native-shell build with the gated flows removed:
VITE_CAP_TOKEN=false VITE_CAP_MARKETPLACE=false VITE_CAP_WALLET=false npm run build
```

## Auth / identity (trusted-header, gateway-driven)

The PWA does **not** implement authentication. A Kong/OPA edge terminates OIDC,
validates the token, and injects **trusted identity headers** upstream. The
client only:

- reads a backend session ("who am I") endpoint that echoes the edge-asserted
  identity/tenant (`VITE_SESSION_ENDPOINT`, default `/api/session`), and
- redirects to the gateway sign-in entry (`VITE_GATEWAY_SIGNIN_URL`, default
  `/oauth2/start`) for the OIDC hand-off.

There is **no** JWT parsing, password handling, or OAuth client logic in the
bundle. The pieces:

- `src/config/session.ts` — endpoint/URL config from `import.meta.env`.
- `src/auth/session.ts` — `fetchSession()` (401/403 ⇒ anonymous), sign-in URL
  builder, and `apiFetch()` (a `fetch` wrapper that routes to `/login` on a 401,
  giving every later view the session-expiry redirect).
- `src/auth/SessionProvider.tsx` — one session check shared via context.
- `src/auth/RequireSession.tsx` — route guard: anonymous/expired ⇒ `/login`.
- `src/views/LoginView.tsx` — the entry view: shows identity/tenant when signed
  in, otherwise directs to the gateway.

`/login` sits outside the guard and the tab-bar shell; every other route is
behind `RequireSession`. See `.env.example` for the full config surface.

## Capacitor

`base: './'` (relative asset URLs) + hash routing keep the bundle
shell-agnostic. `capacitor.config.json` points `webDir` at `dist/`; a native
shell wraps that bundle without code changes.
