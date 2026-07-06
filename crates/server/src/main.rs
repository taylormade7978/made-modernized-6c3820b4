//! Authoritative game server for MADE.
//!
//! This binary is a *driving adapter* on the outside of the hexagon. It exposes
//! two surfaces over the same domain core:
//!
//! * the authoritative WebSocket match channel ([`ws`]) on `/ws` — an `actix-ws`
//!   endpoint that drives the [`GameSession`](game_session::GameSession)
//!   aggregate server-side as the source of truth, validates client actions with
//!   the shared rules crate, broadcasts state deltas, keeps live state in Redis,
//!   and seals a `MatchReplay` to PostgreSQL when a match completes; and
//! * the versioned `/v1` REST API ([`http`]) — collection/deck, leaderboard/
//!   ranked, shop-payments, and catalog endpoints over the Postgres repository
//!   adapters.
//!
//! Both surfaces share one lazily-connected Postgres pool, so the process binds
//! and serves its liveness probe immediately without waiting on Postgres, Redis,
//! or the Kong/OPA sidecars. Auth is terminated by those sidecars upstream, so
//! there is no auth middleware here — the handlers only read the identity the
//! gateway injects.

mod http;
mod ws;

use actix_web::{get, web, App, HttpResponse, HttpServer, Responder};

/// Liveness probe.
#[get("/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().body("ok")
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Build the Postgres pool both surfaces run over. It connects *lazily* — the
    // server binds and serves immediately, and the first request that touches the
    // database establishes the connection — so startup does not depend on
    // Postgres (or the Kong/OPA sidecars) already being reachable.
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://made:made@localhost:5432/made".to_string());
    let pool = persistence::connect_lazy(&database_url)
        .expect("DATABASE_URL must be a valid Postgres connection string");

    // Connect the ephemeral Redis handle for live match state. This fails *soft*:
    // an unreachable Redis disables live-state persistence (which is ephemeral and
    // safe to lose) rather than blocking startup.
    let redis = ws::connect_redis().await;
    if redis.is_some() {
        println!("live match state: Redis connected");
    } else {
        println!("live match state: Redis unavailable, running in-memory only");
    }

    let api_state = web::Data::new(http::ApiState::new(pool.clone()));
    let ws_state = web::Data::new(ws::WsState::new(pool, redis));

    let addr = ("127.0.0.1", 8080);
    println!(
        "MADE game server listening on http://{}:{} (REST /v1, ws /ws)",
        addr.0, addr.1
    );

    HttpServer::new(move || {
        App::new()
            .app_data(api_state.clone())
            .app_data(ws_state.clone())
            // Malformed JSON bodies render the same structured 400 envelope as a
            // failed field validation.
            .app_data(http::json_config())
            .service(health)
            .route("/ws", web::get().to(ws::game_ws))
            .configure(http::configure)
    })
    .bind(addr)?
    .run()
    .await
}
