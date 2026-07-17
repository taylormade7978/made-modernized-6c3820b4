//! API-level integration test for the authoritative WebSocket match surface.
//!
//! A full match is played to completion over the same authoritative
//! [`MatchHub`](server::ws::hub::MatchHub) the `/ws` socket drives, running the
//! *exact* durable side-effect path the socket handler runs on every accepted
//! action ([`persist_applied`]) — mirroring live state to a real Redis and
//! sealing a `MatchReplay` to a real Postgres on completion. The test then
//! asserts three things the acceptance criteria call out:
//!
//! * the completed match **persists a `MatchReplay`** (sealed, with its
//!   checksum) and a `Completed` `game_sessions` row, retrievable via the
//!   repository adapters;
//! * live state is **mirrored to Redis** during play and **cleared on seal**;
//! * replaying the **same seed + command stream reproduces the same result** —
//!   a byte-identical delta stream and the same sealed checksum.
//!
//! Postgres is provisioned per-test by `#[sqlx::test]`; Redis is used when
//! `MADE_REDIS_URL` is set (the `api-integration` CI job sets it). Gated behind
//! the `integration-tests` feature.
#![cfg(feature = "integration-tests")]

mod common;

use serde_json::Value;

use game_session::{ConcedeMatch, StartMatch};
use persistence::repositories::match_play::{GameSessionRepository, MatchReplayRepository};
use persistence::PgPool;
use server::ws::hub::{ApplyOutcome, MatchHub};
use server::ws::protocol::MatchSnapshot;
use server::ws::{persist_applied, WsState};

/// The authoritative command stream for one full match, deterministic in the
/// given `seed`: open the match (player `A` to move), then have `B` concede —
/// forfeiting the match to `A`. Concede is turn-exempt, so `B` may forfeit on
/// `A`'s opening turn, and the winner (`A`) is a seed-robust outcome of this
/// stream (only the sealed checksum, which embeds the seed via the
/// `match.started` frame, varies across seeds).
///
/// NOTE (Task 6): Summon effect resolution now puts units on the board, but a
/// summoned unit arrives with summoning sickness that no command in the current
/// set clears, and `B`'s Boss opens at full HP — so a *lethal attack* cannot
/// drive a match to completion here yet. This concede stream is the seed-robust
/// completion the cross-seed replay assertion below requires; restore an
/// attack-driven completion once a turn-start readiness step and a reachable
/// lethal line exist. Summon resolution itself is covered by the
/// `game-session` unit tests.
fn match_script(match_id: &str, seed: u64) -> Vec<(&'static str, Value)> {
    let outfit_b = format!("{match_id}-b");
    vec![
        (
            StartMatch::COMMAND,
            serde_json::to_value(StartMatch::new(
                match_id,
                format!("{match_id}-a"),
                format!("{match_id}-b"),
                seed,
            ))
            .unwrap(),
        ),
        (
            ConcedeMatch::COMMAND,
            serde_json::to_value(ConcedeMatch::new(match_id, outfit_b)).unwrap(),
        ),
    ]
}

/// The acting player id for a script step: the authenticated envelope identity
/// the socket handler would carry. These full-command payloads already embed the
/// acting `playerId` (except `StartMatch`, which names no acting player — the
/// host stands in, and the hub stamps a harmless `playerId` the aggregate
/// ignores), so reading it back mirrors what the real `/ws` envelope supplies.
fn actor_of(payload: &Value) -> &str {
    payload
        .get("playerId")
        .and_then(Value::as_str)
        .unwrap_or("host-player")
}

/// Play the whole script against a fresh in-memory hub (no IO), returning the
/// sealed checksum, the winning seat, and the ordered `(seq, event_type)` delta
/// stream — the reference the live run is reproduced against.
fn play_pure(match_id: &str, seed: u64) -> (String, String, Vec<(u64, String)>) {
    let hub = MatchHub::new();
    hub.join(match_id, "host-player", None);
    let mut deltas = Vec::new();
    let mut checksum = String::new();
    let mut winner = String::new();
    for (command, payload) in match_script(match_id, seed) {
        match hub.apply_action(match_id, command, actor_of(&payload), &payload) {
            ApplyOutcome::Applied(applied) => {
                for d in &applied.new_deltas {
                    deltas.push((d.seq, d.event_type.clone()));
                }
                if let Some(c) = applied.completion {
                    checksum = c.checksum;
                    winner = c.winner;
                }
            }
            other => panic!("{command} rejected during pure replay: {other:?}"),
        }
    }
    (checksum, winner, deltas)
}

#[sqlx::test(migrator = "persistence::MIGRATOR")]
async fn full_ws_match_persists_replay_and_reproduces_from_seed(pool: PgPool) {
    const SEED: u64 = 0x00C0_FFEE;
    let match_id = "m-integration";

    let redis = common::redis_handle("wsmatch").await;
    let state = WsState::new(pool.clone(), redis.clone());

    // A client joins the authoritative channel first (as the socket handler does
    // on a `join` frame); a fresh match has an empty backlog.
    let join = state.hub.join(match_id, "host-player", None);
    assert!(join.backlog.is_empty());
    assert_eq!(join.since_seq, 0);

    // Drive the full match, running the real durable side effects each step.
    let mut applied_deltas: Vec<(u64, String)> = Vec::new();
    let mut completion = None;
    for (command, payload) in match_script(match_id, SEED) {
        let applied = match state
            .hub
            .apply_action(match_id, command, actor_of(&payload), &payload)
        {
            ApplyOutcome::Applied(applied) => applied,
            other => panic!("{command} was not applied: {other:?}"),
        };
        for d in &applied.new_deltas {
            applied_deltas.push((d.seq, d.event_type.clone()));
        }
        let terminal = applied.completion.is_some();

        // The exact path `/ws` runs on every accepted action.
        persist_applied(&state, match_id, applied.clone()).await;

        // While the match is live, its snapshot is mirrored to Redis (if present).
        if let Some(redis) = &redis {
            if !terminal {
                let bytes = redis
                    .match_state()
                    .read_snapshot(match_id)
                    .await
                    .unwrap()
                    .expect("live snapshot mirrored to Redis mid-match");
                let snap: MatchSnapshot = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(snap.sequence as usize, applied_deltas.len());
                assert!(!snap.completed);
            }
        }

        if terminal {
            completion = applied.completion;
        }
    }

    let completion = completion.expect("the match reached a terminal state");
    assert_eq!(
        completion.winner, "A",
        "B conceding forfeits the match to seat A"
    );

    // ---- The sealed MatchReplay is persisted and retrievable ----
    assert!(
        state.hub.has_replay(match_id),
        "sealed replay retrievable via the repository port"
    );

    let replay = MatchReplayRepository::new(pool.clone())
        .find_by_id(match_id)
        .await
        .expect("query the replay")
        .expect("a MatchReplay row was persisted to Postgres");
    assert!(replay.sealed, "the persisted replay is sealed");
    assert_eq!(
        replay.checksum.as_deref(),
        Some(completion.checksum.as_str()),
        "the persisted checksum is the match's deterministic digest"
    );

    let session = GameSessionRepository::new(pool.clone())
        .find_by_id(match_id)
        .await
        .expect("query the session")
        .expect("a game_sessions row was persisted");
    assert_eq!(session.status, "Completed");
    assert_eq!(session.host_player_id, "host-player");

    // ---- Live state was cleared from Redis once the durable replay is truth ----
    if let Some(redis) = &redis {
        let after = redis.match_state().read_snapshot(match_id).await.unwrap();
        assert!(
            after.is_none(),
            "the live snapshot is dropped from Redis after completion"
        );
    }

    // ---- Deterministic reproduction from the same seed ----
    let (replay_checksum, replay_winner, replay_deltas) = play_pure(match_id, SEED);
    assert_eq!(
        replay_deltas, applied_deltas,
        "the same seed + command stream reproduces the delta stream byte-for-byte"
    );
    assert_eq!(
        replay_checksum, completion.checksum,
        "the sealed checksum is reproduced from the seed"
    );
    assert_eq!(replay_winner, completion.winner);

    // The winner is an RNG-free outcome of this stream, so a different seed still
    // completes the same way — while the sealed checksum embeds the seed (via the
    // `match.started` frame) and so is not expected to match across seeds.
    let (other_checksum, other_winner, _) = play_pure(match_id, 0x1234_5678);
    assert_eq!(other_winner, completion.winner);
    assert_ne!(
        other_checksum, completion.checksum,
        "a different seed yields a different sealed replay"
    );
}
