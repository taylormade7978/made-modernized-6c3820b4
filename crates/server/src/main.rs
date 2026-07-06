//! Authoritative WebSocket game server for MADE.
//!
//! This binary is a *driving adapter* on the outside of the hexagon: it accepts
//! WebSocket connections (via `actix-ws`) and HTTP requests (via `actix-web`),
//! translates them into domain [`Command`]s, and dispatches them to aggregates
//! through their `execute` contract. Persistence is provided by the in-memory
//! mock repositories from the `mocks` crate; a real store can be swapped in
//! later without touching the domain because both satisfy the same
//! [`shared::Repository`] port.
//!
//! At this scaffold stage the aggregates recognize no commands, so the server
//! faithfully reports the domain's [`shared::DomainError::UnknownCommand`]
//! decision back to the client.
//!
//! Alongside the WebSocket channel it serves the versioned `/v1` REST API (see
//! [`http`]): collection/deck, leaderboard/ranked, shop-payments, and catalog
//! endpoints wired to the Postgres repository adapters. Auth is terminated by
//! the Kong/OPA sidecars in front of this process, so the REST handlers only
//! read the identity the gateway injects — there is no auth middleware here.

mod http;

use std::sync::Mutex;

use actix_web::{get, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use futures_util::StreamExt;

use game_session::GameSession;
use mocks::InMemoryGameSessionRepository;
use shared::{Aggregate, Command};

/// Shared application state: the persistence adapters wired into the server.
///
/// A `Mutex` keeps the scaffold simple; a real deployment would use a
/// concurrency-friendly store or actor-per-session model.
struct AppState {
    game_sessions: Mutex<InMemoryGameSessionRepository>,
}

/// Liveness probe.
#[get("/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().body("ok")
}

/// Authoritative WebSocket endpoint. Each inbound text frame is treated as a
/// command name dispatched to a `GameSession`; the server echoes back the
/// domain's decision (currently always `UnknownCommand`, per the stub).
async fn game_ws(
    req: HttpRequest,
    body: web::Payload,
    state: web::Data<AppState>,
) -> Result<HttpResponse, actix_web::Error> {
    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, body)?;

    actix_web::rt::spawn(async move {
        while let Some(Ok(msg)) = msg_stream.next().await {
            match msg {
                actix_ws::Message::Text(text) => {
                    let reply = dispatch(&state, text.to_string());
                    if session.text(reply).await.is_err() {
                        break;
                    }
                }
                actix_ws::Message::Ping(bytes) => {
                    let _ = session.pong(&bytes).await;
                }
                actix_ws::Message::Close(reason) => {
                    let _ = session.close(reason).await;
                    break;
                }
                _ => {}
            }
        }
    });

    Ok(response)
}

/// Run a command name against a fresh `GameSession` and render the outcome.
fn dispatch(state: &AppState, command_name: String) -> String {
    // Touch the repository so the wiring is exercised, not just declared.
    let _sessions = state.game_sessions.lock().expect("session store poisoned");

    let mut session = GameSession::new("live");
    match session.execute(Command::new(command_name)) {
        Ok(_events) => "ok".to_string(),
        Err(err) => err.to_string(),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let state = web::Data::new(AppState {
        game_sessions: Mutex::new(InMemoryGameSessionRepository::new()),
    });

    // Build the Postgres pool the `/v1` REST handlers run over. It connects
    // *lazily* — the server binds and serves immediately, and the first request
    // that touches the database establishes the connection — so startup does not
    // depend on Postgres (or the Kong/OPA sidecars) already being reachable.
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://made:made@localhost:5432/made".to_string());
    let pool = persistence::connect_lazy(&database_url)
        .expect("DATABASE_URL must be a valid Postgres connection string");
    let api_state = web::Data::new(http::ApiState::new(pool));

    let addr = ("127.0.0.1", 8080);
    println!(
        "MADE game server listening on http://{}:{} (REST /v1, ws /ws)",
        addr.0, addr.1
    );

    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .app_data(api_state.clone())
            // Malformed JSON bodies render the same structured 400 envelope as a
            // failed field validation.
            .app_data(http::json_config())
            .service(health)
            .route("/ws", web::get().to(game_ws))
            .configure(http::configure)
    })
    .bind(addr)?
    .run()
    .await
}
