.PHONY: build test wasm run fmt clippy check migrate sqlx-prepare web-image web-run server-image server-run content-validate helm-lint

# Build every crate and target (including test targets).
build:
	cargo build --workspace --all-targets

# Compile test targets without running, then run the full suite.
test:
	cargo test --workspace --no-run
	cargo test --workspace

# Compile the GameSession rules crate to WASM for the browser client.
# Requires: rustup target add wasm32-unknown-unknown (and wasm-pack for pkg output).
wasm:
	wasm-pack build crates/game-session -- --features wasm

# Run the authoritative WebSocket game server.
run:
	cargo run -p server --bin made-server

# Build the production PWA container image (compiles the rules-WASM, runs the
# Vite build, serves via hardened rootless NGINX). Context is the repo root
# because the wasm crate lives outside web/.
web-image:
	docker build -f web/Dockerfile -t made-pwa:local .

# Run the image locally on http://localhost:8080 (health check at /healthz).
web-run: web-image
	docker run --rm -p 8080:8080 made-pwa:local

# Build the production backend image (`made-server`): cargo release build against
# the committed sqlx offline metadata, into a slim non-root Debian runtime.
# Context is the repo root because the build needs the whole workspace, .sqlx/,
# and migrations/.
server-image:
	docker build -f crates/server/Dockerfile -t made-server:local .

# Run the backend image locally on http://localhost:8080 (health at /health,
# Prometheus scrape at /metrics). Postgres/Redis connect lazily / fail soft, so
# it serves its probes with no backing services in reach.
server-run: server-image
	docker run --rm -p 8080:8080 made-server:local

# Apply the sqlx migrations to $DATABASE_URL (local dev + CI share these files).
# e.g. DATABASE_URL=postgres://made:made@localhost:5432/made make migrate
migrate:
	cargo run -p persistence --bin made-migrate

# Regenerate the committed offline query metadata (.sqlx/) after changing a
# compile-time-checked query. Requires a live DATABASE_URL at the target schema.
sqlx-prepare:
	cargo sqlx prepare --workspace

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

# Validate the shipped content catalog (content/catalog/*.json) against the
# domain's own card invariants. CI runs this on every PR; run it locally after
# editing catalog data.
content-validate:
	cargo run -q -p domain --bin content-validator -- content/catalog

# Render + lint the Helm chart for both overlays without a cluster (what CI
# runs on PRs). Requires helm on PATH.
helm-lint:
	helm lint deploy/helm/made -f deploy/helm/made/values-dev.yaml
	helm lint deploy/helm/made -f deploy/helm/made/values-prod.yaml
	helm template made deploy/helm/made -f deploy/helm/made/values-prod.yaml >/dev/null

# The local compile gate: build all targets, then compile + run tests.
check: build test
