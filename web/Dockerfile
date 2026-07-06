# syntax=docker/dockerfile:1.7
#
# Multi-stage production build for the MADE PWA (web/) → made.vforce360.ai.
#
#   1. wasm    — compile the shared rules crate to WASM with wasm-pack.
#   2. web     — Vite production build, consuming the WASM pkg from stage 1.
#   3. runtime — hardened, rootless NGINX serving the immutable static bundle.
#
# Build context is the REPO ROOT (the wasm crate lives outside web/), so build
# with:  `docker build -f web/Dockerfile -t made-pwa .`
#
# A `dist` stage is exposed for CI to export the versioned OTA bundle without a
# runtime image:  `docker buildx build -f web/Dockerfile --target dist \
#   -o type=local,dest=ota-dist .`

# ── Stage 1: rules-WASM ───────────────────────────────────────────────────────
# Compile crates/game-session to WASM. Only shared + game-session actually build
# for wasm32, but the whole workspace is copied so cargo can resolve the members
# list in the root manifest.
FROM rust:1-bookworm AS wasm
WORKDIR /src

RUN rustup target add wasm32-unknown-unknown \
    && curl -fsSL https://rustwasm.github.io/wasm-pack/installer/init.sh | sh

# Manifests first for layer caching, then sources.
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY crates ./crates

# `--target web` emits an ES module + a co-located .wasm that inits via
# `new URL('game_session_bg.wasm', import.meta.url)` — see web/index.html's
# import map and web/scripts/stage-wasm.mjs.
RUN wasm-pack build crates/game-session --release --target web \
    --out-dir pkg -- --features wasm

# ── Stage 2: Vite build ───────────────────────────────────────────────────────
FROM node:22-bookworm-slim AS web
WORKDIR /app/web

# Capability flags are build-time gating (native-shell vs open-web builds); pass
# them as build args so a store-policy build can strip the token/wallet flows.
ARG VITE_CAP_TOKEN
ARG VITE_CAP_MARKETPLACE
ARG VITE_CAP_WALLET
ENV VITE_CAP_TOKEN=${VITE_CAP_TOKEN} \
    VITE_CAP_MARKETPLACE=${VITE_CAP_MARKETPLACE} \
    VITE_CAP_WALLET=${VITE_CAP_WALLET}

# Deterministic install from the committed lockfile (layer-cached on lockfile).
COPY web/package.json web/package-lock.json ./
RUN npm ci

# The WASM pkg must sit at ../crates/game-session/pkg relative to web/ so the
# `prebuild` hook (scripts/stage-wasm.mjs) copies it into public/vendor/.
COPY --from=wasm /src/crates/game-session/pkg /app/crates/game-session/pkg

# App sources.
COPY web/ ./

# `npm run build` runs prebuild (stage-wasm) → tsc --noEmit → vite build → dist/.
RUN npm run build

# ── Stage 2b: OTA bundle export target ────────────────────────────────────────
# A minimal, filesystem-only stage whose entire content IS the built bundle, so
# CI can export it verbatim for OTA publishing (`--target dist -o type=local`).
FROM scratch AS dist
COPY --from=web /app/web/dist /

# ── Stage 3: hardened runtime ─────────────────────────────────────────────────
# nginx-unprivileged runs as a non-root user (uid 101) and listens on 8080 — no
# CAP_NET_BIND_SERVICE, no root worker. Static bundle only; no app secrets.
FROM nginxinc/nginx-unprivileged:1.27-alpine AS runtime

# Provenance labels; OTA_VERSION is stamped by CI (see .github/workflows/ci.yml).
ARG OTA_VERSION=dev
LABEL org.opencontainers.image.title="made-pwa" \
      org.opencontainers.image.description="MADE card-game PWA static bundle (made.vforce360.ai)" \
      org.opencontainers.image.source="https://github.com/edgentx/made-modernized-6c3820b4" \
      org.opencontainers.image.version="${OTA_VERSION}"

# Hardened server config (security headers, wasm MIME, SPA fallback, /healthz).
COPY web/nginx.conf /etc/nginx/conf.d/default.conf

# The immutable static bundle.
COPY --from=web /app/web/dist /usr/share/nginx/html

EXPOSE 8080

# Liveness: the config serves a cheap 200 at /healthz. busybox wget ships in the
# alpine base, so no extra layer is needed.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget -q -O /dev/null http://127.0.0.1:8080/healthz || exit 1
