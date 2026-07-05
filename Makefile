.PHONY: build test wasm run fmt clippy check

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

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets

# The local compile gate: build all targets, then compile + run tests.
check: build test
