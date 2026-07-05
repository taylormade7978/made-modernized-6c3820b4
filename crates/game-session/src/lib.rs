//! GameSession bounded context — the authoritative card-game rules aggregate.
//!
//! This crate is special among the bounded contexts: it compiles both natively
//! (linked by the actix-web server for authoritative server-side execution) and
//! to `wasm32` via `wasm-pack`, so the browser client can run the *same* rules
//! for prediction/validation. To keep that dual target working, the crate
//! depends only on the WASM-safe [`shared`] kernel; the browser entrypoint is
//! isolated behind the optional `wasm` feature.
//!
//! Build for the web:
//! `wasm-pack build crates/game-session -- --features wasm`

// The GameSession aggregate is scaffolded with the same stub contract as the
// other bounded contexts (see `shared::stub_aggregate!`): it embeds
// `AggregateRoot` for version + uncommitted-event tracking and its `execute`
// currently recognizes no commands, returning `UnknownCommand`.
shared::stub_aggregate!(GameSession, GameSessionRepository);

/// WASM bindings for shared server/client rule execution.
///
/// Only compiled when the `wasm` feature is enabled (i.e. under `wasm-pack`),
/// so the native host build never pulls in `wasm-bindgen`.
#[cfg(feature = "wasm")]
mod wasm_bindings {
    use super::GameSession;
    use shared::{Aggregate, Command};
    use wasm_bindgen::prelude::*;

    /// Run a command against a fresh `GameSession` from the browser client.
    ///
    /// Returns `Ok(())` when the command is applied, or the domain error text
    /// (e.g. the `UnknownCommand` message) as a `JsValue` — mirroring exactly
    /// what the authoritative server would decide for the same input.
    #[wasm_bindgen]
    pub fn execute_command(session_id: String, command_name: String) -> Result<(), JsValue> {
        let mut session = GameSession::new(session_id);
        session
            .execute(Command::new(command_name))
            .map(|_events| ())
            .map_err(|err| JsValue::from_str(&err.to_string()))
    }
}
