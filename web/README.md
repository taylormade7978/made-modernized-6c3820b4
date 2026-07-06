# MADE PWA (`web/`)

Mobile-first **React + TypeScript** PWA client for the MADE card game
(VForce360 Track B). Vite build, client-side routing, a service worker + web
app manifest for offline / OTA bundles, and a **shell-agnostic** bundle that
runs standalone in a browser and, unchanged, inside a Capacitor native
container.

## Commands

```sh
npm install
npm run dev      # Vite dev server
npm run build    # tsc --noEmit && vite build → dist/ (manifest + service worker)
npm run preview  # serve the production build locally
npm run icons    # regenerate PWA raster icons (public/icons/*.png)
```

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
