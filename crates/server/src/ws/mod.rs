//! The authoritative WebSocket match-sync endpoint (`ws.{project}.vforce360.ai`).
//!
//! This is the driving adapter S-77 adds on top of the REST surface: an
//! `actix-ws` channel that drives the [`GameSession`](game_session::GameSession)
//! aggregate server-side as the single source of truth. It is a thin IO shell
//! around the pure [`hub::MatchHub`] — the shell owns only the async concerns
//! (the socket, Redis, PostgreSQL); every rules decision lives in the hub.
//!
//! # Per-connection lifecycle
//!
//! Each socket runs one task. It reads client frames ([`protocol::ClientMessage`])
//! and:
//!
//! * **join** — reads any live [`protocol::MatchSnapshot`] from Redis, subscribes
//!   the client through [`hub::MatchHub::join`], replays the authoritative backlog
//!   as [`protocol::ServerMessage::Joined`], and spawns a forwarder that pushes
//!   every subsequent broadcast delta to this socket. A reconnecting client
//!   simply re-joins and is re-served the backlog — its session is never
//!   corrupted (acceptance criterion).
//! * **action** — dispatches the command to [`hub::MatchHub::apply_action`]. The
//!   hub re-runs it against the aggregate; on success the deltas were already
//!   broadcast, so the adapter only performs the durable writes: it mirrors the
//!   live snapshot to Redis, re-publishes the deltas on the match-event channel,
//!   and — when the match completed — persists the sealed [`MatchReplay`] to
//!   PostgreSQL. On a rules rejection it relays the correction to the acting
//!   client alone.
//!
//! Auth is terminated by the Kong/OPA sidecars upstream exactly as for the REST
//! handlers, so there is no auth middleware here.

pub mod hub;
pub mod protocol;
pub mod rng;

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use futures_util::StreamExt;
use tokio::sync::broadcast::error::RecvError;

use ephemeral::{MatchEvent, RedisConfig, RedisHandle};
use persistence::repositories::match_play::{
    GameSessionRepository, GameSessionRow, MatchReplayRepository, MatchReplayRow,
};
use persistence::PgPool;

use hub::{Applied, ApplyOutcome, Completion, MatchHub};
use protocol::{ClientMessage, MatchSnapshot, ServerMessage, StateDelta};

/// Shared state the WebSocket endpoint runs over: the authoritative match hub,
/// an optional Redis handle for live state (absent when Redis is unreachable —
/// live state is ephemeral and safe to lose), and the Postgres pool the sealed
/// replays are persisted to.
#[derive(Clone)]
pub struct WsState {
    /// The single authoritative match registry, shared across every connection.
    pub hub: Arc<MatchHub>,
    /// The live-state store; `None` degrades to in-memory-only live state.
    pub redis: Option<RedisHandle>,
    /// The durable pool the sealed replay is written to on completion.
    pub pool: PgPool,
}

impl WsState {
    /// Build the endpoint state around a Postgres pool and an optional Redis
    /// handle.
    pub fn new(pool: PgPool, redis: Option<RedisHandle>) -> Self {
        Self {
            hub: Arc::new(MatchHub::new()),
            redis,
            pool,
        }
    }
}

/// Attempt to connect the ephemeral Redis handle from the environment, *failing
/// soft*: an unreachable Redis logs a warning and yields `None` rather than
/// aborting startup, because live match state is ephemeral (the durable replay
/// in PostgreSQL is the record of truth) and the endpoint runs in-memory-only
/// without it. This mirrors the lazy-Postgres startup: booting the server never
/// blocks on a sidecar being reachable.
pub async fn connect_redis() -> Option<RedisHandle> {
    let config = match RedisConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("redis config invalid; live match state disabled: {err}");
            return None;
        }
    };
    match ephemeral::connect(&config).await {
        Ok(handle) => Some(handle),
        Err(err) => {
            eprintln!("redis unreachable; live match state disabled: {err}");
            None
        }
    }
}

/// The `/ws` upgrade handler. Spawns the per-connection task and returns the
/// switching-protocols response immediately.
pub async fn game_ws(
    req: HttpRequest,
    body: web::Payload,
    state: web::Data<WsState>,
) -> Result<HttpResponse, actix_web::Error> {
    let (response, session, mut msg_stream) = actix_ws::handle(&req, body)?;
    let state = state.get_ref().clone();

    actix_web::rt::spawn(async move {
        let mut session = session;
        // The forwarder task pushing this match's broadcast deltas to the socket;
        // replaced if the client joins another match, aborted on disconnect.
        let mut forwarder: Option<actix_web::rt::task::JoinHandle<()>> = None;

        while let Some(Ok(msg)) = msg_stream.next().await {
            match msg {
                actix_ws::Message::Text(text) => {
                    handle_frame(&state, &text, &mut session, &mut forwarder).await;
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

        if let Some(handle) = forwarder {
            handle.abort();
        }
    });

    Ok(response)
}

/// Decode and handle one inbound text frame.
async fn handle_frame(
    state: &WsState,
    text: &str,
    session: &mut actix_ws::Session,
    forwarder: &mut Option<actix_web::rt::task::JoinHandle<()>>,
) {
    let message = match serde_json::from_str::<ClientMessage>(text) {
        Ok(message) => message,
        Err(err) => {
            reply(
                session,
                &ServerMessage::Error {
                    message: format!("malformed frame: {err}"),
                },
            )
            .await;
            return;
        }
    };

    match message {
        ClientMessage::Join {
            match_id,
            player_id,
        } => handle_join(state, &match_id, &player_id, session, forwarder).await,
        ClientMessage::Action {
            match_id,
            player_id,
            command,
            payload,
        } => handle_action(state, &match_id, &player_id, &command, &payload, session).await,
    }
}

/// Subscribe the client to a match and start forwarding its deltas.
async fn handle_join(
    state: &WsState,
    match_id: &str,
    player_id: &str,
    session: &mut actix_ws::Session,
    forwarder: &mut Option<actix_web::rt::task::JoinHandle<()>>,
) {
    // Hydrate from Redis live state if this process has never seen the match.
    let snapshot = read_snapshot(state, match_id).await;
    let join = state.hub.join(match_id, player_id, snapshot);

    reply(
        session,
        &ServerMessage::Joined {
            match_id: match_id.to_string(),
            since_seq: join.since_seq,
            backlog: join.backlog,
        },
    )
    .await;

    // Replace any prior forwarder (the client re-joined) and stream deltas.
    if let Some(handle) = forwarder.take() {
        handle.abort();
    }
    let mut receiver = join.receiver;
    let mut sink = session.clone();
    *forwarder = Some(actix_web::rt::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(frame) => {
                    if sink.text(frame).await.is_err() {
                        break;
                    }
                }
                // A lagging subscriber drops missed frames; the join backlog is
                // the recovery path, so keep going rather than tearing down.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    }));
}

/// Dispatch a client command to the hub and perform the durable side effects.
///
/// `player_id` is the acting identity from the authenticated envelope; the hub
/// stamps it (and `match_id`) onto the command body so the client only needs to
/// send the action-specific fields and cannot spoof another player through the
/// payload.
async fn handle_action(
    state: &WsState,
    match_id: &str,
    player_id: &str,
    command: &str,
    payload: &serde_json::Value,
    session: &mut actix_ws::Session,
) {
    match state
        .hub
        .apply_action(match_id, command, player_id, payload)
    {
        ApplyOutcome::Applied(applied) => persist_applied(state, match_id, applied).await,
        // A rejection (optimistic-state correction) or unknown-match error goes
        // only to the acting client; the deltas of an accepted command reach it
        // through its broadcast forwarder.
        ApplyOutcome::Rejected(message) | ApplyOutcome::UnknownMatch(message) => {
            reply(session, &message).await;
        }
    }
}

/// Persist the durable effects of an accepted command: mirror the live snapshot
/// to Redis, re-publish the deltas on the match-event channel, and seal the
/// replay durably when the match completed.
///
/// Public so the API-level integration suite can drive a full match through the
/// hub and then run this *exact* durable path against real Postgres + Redis
/// containers — the same code the `/ws` socket handler runs on every accepted
/// action — rather than re-implementing the persistence side effects in a test.
pub async fn persist_applied(state: &WsState, match_id: &str, applied: Applied) {
    if let Some(redis) = &state.redis {
        // Mirror live state so a reconnect (even in another process) resumes it.
        if let Ok(bytes) = serde_json::to_vec(&applied.snapshot) {
            if let Err(err) = redis
                .match_state()
                .write_snapshot(match_id, &bytes, None)
                .await
            {
                eprintln!("failed to write live snapshot for '{match_id}': {err}");
            }
        }
        // Fan the deltas out to any cross-instance subscribers of the channel.
        let events = redis.events();
        for delta in &applied.new_deltas {
            let event = MatchEvent::new(match_id, &delta.event_type, delta_payload(delta));
            if let Err(err) = events.publish(&event).await {
                eprintln!("failed to publish delta for '{match_id}': {err}");
            }
        }
    }

    if let Some(completion) = applied.completion {
        persist_replay(&state.pool, &completion).await;
        // The sealed replay is now retrievable through the hub's repository port
        // as well as durably in PostgreSQL; record the seal for the audit trail.
        println!(
            "match '{}' sealed at seq {} (winner {}, replay retrievable: {})",
            completion.match_id,
            completion.sealed_sequence,
            completion.winner,
            state.hub.has_replay(&completion.match_id),
        );
        // The sealed match's live snapshot can be dropped from Redis now that the
        // durable replay is the record of truth.
        if let Some(redis) = &state.redis {
            let _ = redis.match_state().clear(match_id).await;
        }
    }
}

/// Read and decode the live snapshot for `match_id` from Redis, if any. A
/// missing Redis handle, an absent key, or a malformed value all degrade to
/// `None` — a fresh match, not an error.
async fn read_snapshot(state: &WsState, match_id: &str) -> Option<MatchSnapshot> {
    let redis = state.redis.as_ref()?;
    match redis.match_state().read_snapshot(match_id).await {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).ok(),
        Ok(None) => None,
        Err(err) => {
            eprintln!("failed to read live snapshot for '{match_id}': {err}");
            None
        }
    }
}

/// Persist a completed match's sealed replay to PostgreSQL through the repository
/// adapters (acceptance criterion: *completed matches persist a MatchReplay
/// record retrievable via repository*).
///
/// Best-effort and idempotent-ish: it ensures the `game_sessions` row the replay
/// references exists, inserts the replay open, then seals it with its checksum.
/// Any step failing is logged, not fatal — the in-memory hub already holds the
/// authoritative sealed replay, and a live database is not a precondition for the
/// match itself.
async fn persist_replay(pool: &PgPool, completion: &Completion) {
    let sessions = GameSessionRepository::new(pool.clone());
    let session_row = GameSessionRow {
        id: completion.match_id.clone(),
        host_player_id: completion.host_player_id.clone(),
        status: "Completed".to_string(),
        version: 0,
    };
    // A pre-existing session row (the match was opened durably earlier) is fine.
    if let Err(err) = sessions.insert(&session_row).await {
        eprintln!(
            "game_sessions row for '{}' not inserted (may already exist): {err}",
            completion.match_id
        );
    }

    let replays = MatchReplayRepository::new(pool.clone());
    let open = MatchReplayRow {
        id: completion.replay_id.clone(),
        session_id: completion.match_id.clone(),
        sealed: false,
        checksum: None,
        frame_uri: None,
        version: 0,
    };
    if let Err(err) = replays.insert(&open).await {
        eprintln!(
            "match_replays row for '{}' not inserted (may already exist): {err}",
            completion.replay_id
        );
    }

    // Seal it with the deterministic checksum, guarding on the opening version.
    let sealed = MatchReplayRow {
        sealed: true,
        checksum: Some(completion.checksum.clone()),
        frame_uri: Some(format!("replay://{}", completion.match_id)),
        ..open
    };
    if let Err(err) = replays.update(&sealed, 0).await {
        eprintln!("failed to seal replay '{}': {err}", completion.replay_id);
    }
}

/// The JSON the delta carries on the Redis match-event channel — the event
/// fields plus the sequence number that orders them.
fn delta_payload(delta: &StateDelta) -> serde_json::Value {
    serde_json::json!({ "seq": delta.seq, "event": delta.event })
}

/// Send one server frame to a single client, ignoring a closed socket.
async fn reply(session: &mut actix_ws::Session, message: &ServerMessage) {
    let frame = serde_json::to_string(message)
        .unwrap_or_else(|e| format!(r#"{{"type":"error","message":"encode failed: {e}"}}"#));
    let _ = session.text(frame).await;
}
