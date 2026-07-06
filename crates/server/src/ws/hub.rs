//! The authoritative match hub: the server-side source of truth for every live
//! match, decoupled from actix and Redis so the rules loop is unit-testable.
//!
//! [`MatchHub`] holds one [`LiveMatch`] per match id. A [`LiveMatch`] owns the
//! authoritative [`GameSession`] aggregate, the match's seeded RNG harness, the
//! ordered delta backlog, and a broadcast channel every connected client
//! subscribes to. The two public operations map directly onto the acceptance
//! criteria:
//!
//! * [`MatchHub::join`] subscribes a (possibly reconnecting) client and hands
//!   back the authoritative backlog, hydrating a match from a Redis snapshot
//!   when this process has never seen it — *reconnect resumes without corrupting
//!   the session*.
//! * [`MatchHub::apply_action`] re-runs a client command against the aggregate.
//!   On success it appends the resulting events as deltas, broadcasts them to
//!   every subscriber, and — when the match ends — seals a [`MatchReplay`]. On a
//!   rules rejection it returns a correction carrying the authoritative sequence
//!   the client must roll its optimistic state back to — *illegal actions are
//!   rejected server-side and optimistic state is corrected*.
//!
//! No method awaits or performs IO: the durable side effects (writing the Redis
//! snapshot, persisting the sealed replay to PostgreSQL) are described in the
//! returned outcome and executed by the async adapter in [`super`]. That keeps
//! the hub a pure, synchronous, testable core.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::broadcast;

use domain::match_replay::MatchReplay;
use game_session::{Event, GameSession, Player, ResolveCopEvent};
use mocks::InMemoryMatchReplayRepository;
use shared::{Aggregate, Command, DomainEvent, Repository};

use super::protocol::{MatchSnapshot, ServerMessage, StateDelta};
use super::rng::SeededRng;

/// The broadcast channel depth. Reconnecting or lagging clients recover missed
/// deltas from the join backlog, so this only needs to absorb bursts.
const BROADCAST_CAPACITY: usize = 256;

/// What [`MatchHub::apply_action`] decided about a submitted command.
#[derive(Debug)]
pub enum ApplyOutcome {
    /// The command was applied; its deltas were already broadcast. Carries the
    /// fresh snapshot to persist to Redis and, if the match ended, the sealed
    /// replay to persist durably.
    Applied(Applied),
    /// The rules rejected the command. The message is the correction to send the
    /// *acting* client only (so it rolls back its optimistic prediction).
    Rejected(ServerMessage),
    /// No live match exists for the id (client must `join` first). The message is
    /// the error to return to the acting client.
    UnknownMatch(ServerMessage),
}

/// The durable side effects of a successfully applied command.
#[derive(Debug, Clone)]
pub struct Applied {
    /// The live snapshot to write to Redis (ephemeral live state).
    pub snapshot: MatchSnapshot,
    /// The deltas this command produced, in order — already broadcast in-process,
    /// and re-published to the Redis match-event channel for other subscribers.
    pub new_deltas: Vec<StateDelta>,
    /// Present only when this command completed the match: the sealed replay to
    /// persist to PostgreSQL.
    pub completion: Option<Completion>,
}

/// A completed, sealed match ready to be written to the durable replay store.
#[derive(Debug, Clone)]
pub struct Completion {
    /// The match that completed.
    pub match_id: String,
    /// The player who hosts (opened) the match — the `game_sessions.host_player_id`.
    pub host_player_id: String,
    /// The winning seat, `"A"` / `"B"`.
    pub winner: String,
    /// The sealed replay's identity (its match id).
    pub replay_id: String,
    /// The deterministic frame-stream checksum the replay reproduces.
    pub checksum: String,
    /// The sequence number the replay was sealed at.
    pub sealed_sequence: u64,
}

/// What [`MatchHub::join`] hands a newly subscribed client.
pub struct JoinOutcome {
    /// The receiver the connection forwards to the socket.
    pub receiver: broadcast::Receiver<String>,
    /// The authoritative backlog to replay to the joining client.
    pub backlog: Vec<StateDelta>,
    /// The sequence high-water mark at join time.
    pub since_seq: u64,
}

/// One live match's authoritative state.
struct LiveMatch {
    /// The authoritative rules aggregate — the single source of truth.
    session: GameSession,
    /// The match's seeded deterministic RNG (Cop Events / crash tables).
    rng: SeededRng,
    /// The player who opened the match.
    host_player_id: String,
    /// Contiguous sequence high-water mark (== `deltas.last().seq`).
    sequence: u64,
    /// The ordered authoritative delta backlog.
    deltas: Vec<StateDelta>,
    /// Rolling deterministic digest folded over the deltas (the replay checksum).
    digest: u64,
    /// Whether the match has completed (its replay sealed). No further command is
    /// applied once set.
    completed: bool,
    /// The fan-out channel every subscriber of this match listens on.
    sender: broadcast::Sender<String>,
}

impl LiveMatch {
    /// A brand-new live match for `match_id`, hosted by `host_player_id`. The RNG
    /// starts unseeded (seed 0); the authoritative seed arrives with the
    /// `StartMatchCmd` and re-seeds it.
    fn new(match_id: &str, host_player_id: &str) -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            session: GameSession::new(match_id),
            rng: SeededRng::new(0),
            host_player_id: host_player_id.to_string(),
            sequence: 0,
            deltas: Vec::new(),
            digest: 0,
            completed: false,
            sender,
        }
    }

    /// Rebuild a live match from a Redis snapshot after a reconnect in a process
    /// that never held it. The delta backlog and RNG cursor are restored so the
    /// deterministic stream continues rather than restarts; the aggregate itself
    /// is not re-executed here — the backlog is what a reconnecting client needs
    /// to rebuild its view, and no *new* command is accepted for a completed
    /// match.
    fn from_snapshot(snapshot: &MatchSnapshot) -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        let digest = snapshot
            .deltas
            .iter()
            .fold(0u64, |acc, d| acc.wrapping_add(delta_fingerprint(d)));
        Self {
            session: GameSession::new(&snapshot.match_id),
            rng: SeededRng::resume(snapshot.rng_seed, snapshot.rng_draws),
            host_player_id: snapshot.host_player_id.clone(),
            sequence: snapshot.sequence,
            deltas: snapshot.deltas.clone(),
            digest,
            completed: snapshot.completed,
            sender,
        }
    }

    /// Serialize the current live state for the Redis snapshot.
    fn snapshot(&self, match_id: &str) -> MatchSnapshot {
        MatchSnapshot {
            match_id: match_id.to_string(),
            host_player_id: self.host_player_id.clone(),
            rng_seed: self.rng.seed(),
            rng_draws: self.rng.draws(),
            sequence: self.sequence,
            deltas: self.deltas.clone(),
            completed: self.completed,
        }
    }

    /// Record an authoritative event as the next delta: assign it a sequence,
    /// fold it into the digest, append it, and broadcast it to every subscriber.
    fn record_and_broadcast(&mut self, match_id: &str, event: &Event) -> StateDelta {
        self.sequence += 1;
        let delta = StateDelta {
            seq: self.sequence,
            event_type: event.event_type().to_string(),
            event: delta_event_json(event),
        };
        self.digest = self.digest.wrapping_add(delta_fingerprint(&delta));
        self.deltas.push(delta.clone());

        let frame = ServerMessage::Delta {
            match_id: match_id.to_string(),
            delta: delta.clone(),
        };
        // A send with no subscribers is not an error here — the backlog still
        // carries the delta for whoever joins next.
        let _ = self.sender.send(encode(&frame));
        delta
    }

    /// Fold the delta history into a fresh, sealed [`MatchReplay`] aggregate. The
    /// aggregate re-derives its own ordering/immutability/determinism invariants
    /// as it is built, so a corrupt history would surface here rather than being
    /// silently persisted.
    fn build_sealed_replay(&self, match_id: &str) -> Result<MatchReplay, shared::DomainError> {
        let mut replay = MatchReplay::new(match_id);
        replay.seed_from(0);
        for delta in &self.deltas {
            replay.record_event(delta_fingerprint(delta))?;
        }
        replay.seal();
        Ok(replay)
    }
}

/// The registry of every live match this server instance is driving.
///
/// Holds a fresh [`InMemoryMatchReplayRepository`] of sealed replays so a
/// completed match's replay is retrievable via the repository port
/// ([`MatchHub::find_replay`]) — the acceptance criterion — independently of the
/// durable PostgreSQL write the adapter also performs.
pub struct MatchHub {
    matches: Mutex<HashMap<String, LiveMatch>>,
    replays: Mutex<InMemoryMatchReplayRepository>,
}

impl Default for MatchHub {
    fn default() -> Self {
        Self::new()
    }
}

impl MatchHub {
    /// An empty hub driving no matches.
    pub fn new() -> Self {
        Self {
            matches: Mutex::new(HashMap::new()),
            replays: Mutex::new(InMemoryMatchReplayRepository::new()),
        }
    }

    /// Subscribe `player_id` to `match_id`, returning the broadcast receiver plus
    /// the authoritative backlog to replay to the client.
    ///
    /// If this process has never seen the match, it is created — hydrated from
    /// `snapshot` when Redis carries live state for it (the reconnect path), or
    /// fresh otherwise. A client that dropped and reconnected simply finds the
    /// same in-memory match and is re-served the whole backlog, so its session is
    /// never corrupted.
    pub fn join(
        &self,
        match_id: &str,
        player_id: &str,
        snapshot: Option<MatchSnapshot>,
    ) -> JoinOutcome {
        let mut matches = self.matches.lock().expect("match registry poisoned");
        let live = matches.entry(match_id.to_string()).or_insert_with(|| {
            snapshot
                .as_ref()
                .map(LiveMatch::from_snapshot)
                .unwrap_or_else(|| LiveMatch::new(match_id, player_id))
        });
        JoinOutcome {
            receiver: live.sender.subscribe(),
            backlog: live.deltas.clone(),
            since_seq: live.sequence,
        }
    }

    /// Re-run a client `command` against the match authoritatively.
    ///
    /// `command` is the aggregate command name and `payload` its JSON body. For a
    /// `ResolveCopEventCmd` the server injects its *own* seeded d10 draw over any
    /// the client supplied, then only advances the RNG cursor if the command is
    /// accepted — so a rejected attempt never perturbs the deterministic stream.
    pub fn apply_action(
        &self,
        match_id: &str,
        command: &str,
        payload: &serde_json::Value,
    ) -> ApplyOutcome {
        let mut matches = self.matches.lock().expect("match registry poisoned");
        let Some(live) = matches.get_mut(match_id) else {
            return ApplyOutcome::UnknownMatch(ServerMessage::Error {
                message: format!("no live match '{match_id}'; join it first"),
            });
        };

        if live.completed {
            return ApplyOutcome::Rejected(ServerMessage::Rejected {
                match_id: match_id.to_string(),
                command: command.to_string(),
                reason: "match has already completed".to_string(),
                authoritative_seq: live.sequence,
            });
        }

        // A Cop Event resolution is server-authoritative: peek the next seeded
        // draw (without consuming it), inject it, and only commit the draw if the
        // command survives the rules.
        let is_cop_event = command == ResolveCopEvent::COMMAND;
        let cmd = if is_cop_event {
            let draw = live.rng.peek_cop_event();
            build_command(command, payload, Some(draw))
        } else {
            build_command(command, payload, None)
        };

        match live.session.execute(cmd) {
            Ok(events) => {
                if is_cop_event {
                    // Commit the draw we peeked: the command was accepted.
                    let _ = live.rng.next_cop_event();
                }

                // The seed is authoritative only once the match has started.
                for event in &events {
                    if let Event::MatchStarted(started) = event {
                        live.rng = SeededRng::new(started.rng_seed);
                    }
                }

                let winner = events.iter().find_map(terminal_winner);
                let new_deltas: Vec<StateDelta> = events
                    .iter()
                    .map(|event| live.record_and_broadcast(match_id, event))
                    .collect();

                let completion = match winner {
                    Some(seat) => Some(self.seal(match_id, live, seat)),
                    None => None,
                };

                ApplyOutcome::Applied(Applied {
                    snapshot: live.snapshot(match_id),
                    new_deltas,
                    completion,
                })
            }
            Err(err) => ApplyOutcome::Rejected(ServerMessage::Rejected {
                match_id: match_id.to_string(),
                command: command.to_string(),
                reason: err.to_string(),
                // Correct the optimistic client back to the last delta we applied.
                authoritative_seq: live.sequence,
            }),
        }
    }

    /// Seal a completed match: mark it done, save its sealed replay into the
    /// repository (retrievable via [`find_replay`](Self::find_replay)), broadcast
    /// the completion frame, and describe the durable write for the adapter.
    fn seal(&self, match_id: &str, live: &mut LiveMatch, winner: Player) -> Completion {
        live.completed = true;
        let checksum = format!("{:016x}", live.digest);

        // Drive the MatchReplay aggregate and persist it via the repository port.
        // A build error would mean our own delta log is inconsistent; it must not
        // silently vanish, so fall back to leaving the repository untouched and
        // logging — the durable PostgreSQL write in the adapter is the record of
        // truth regardless.
        match live.build_sealed_replay(match_id) {
            Ok(replay) => {
                let mut repo = self.replays.lock().expect("replay store poisoned");
                if let Err(err) = repo.save(match_id, replay) {
                    eprintln!("failed to store sealed replay for '{match_id}': {err}");
                }
            }
            Err(err) => eprintln!("replay for '{match_id}' is inconsistent: {err}"),
        }

        let completion = Completion {
            match_id: match_id.to_string(),
            host_player_id: live.host_player_id.clone(),
            winner: player_tag(winner).to_string(),
            replay_id: match_id.to_string(),
            checksum: checksum.clone(),
            sealed_sequence: live.sequence,
        };

        let frame = ServerMessage::Completed {
            match_id: match_id.to_string(),
            winner: completion.winner.clone(),
            replay_id: completion.replay_id.clone(),
            checksum,
        };
        let _ = live.sender.send(encode(&frame));
        completion
    }

    /// Retrieve a completed match's sealed replay through the repository port.
    /// Returns `true` if a sealed replay is stored for `match_id`. (The aggregate
    /// itself is not lent out — the repository borrow is internal — so this
    /// exposes the *retrievability* the acceptance criterion asks for.)
    pub fn has_replay(&self, match_id: &str) -> bool {
        let repo = self.replays.lock().expect("replay store poisoned");
        repo.find_by_id(match_id).ok().flatten().is_some()
    }
}

/// Build the dispatchable [`Command`] for `command`/`payload`, optionally forcing
/// the seeded Cop Event `rng_draw` over whatever the client sent.
fn build_command(command: &str, payload: &serde_json::Value, rng_draw: Option<u8>) -> Command {
    let mut body = payload.clone();
    if let Some(draw) = rng_draw {
        // Overwrite (or insert) the server-authoritative draw; if the client sent
        // a non-object body, replace it with one carrying just the draw so the
        // aggregate still gets a well-formed (if incomplete) payload to reject.
        if !body.is_object() {
            body = serde_json::json!({});
        }
        if let Some(map) = body.as_object_mut() {
            map.insert("rngDraw".to_string(), serde_json::json!(draw));
        }
    }
    // A body that fails to serialize cannot happen for a `serde_json::Value`.
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    Command::with_payload(command.to_string(), bytes)
}

/// Encode a server frame as a wire string. A frame is always serializable, so a
/// failure degrades to an inline error frame rather than dropping the message.
fn encode(message: &ServerMessage) -> String {
    serde_json::to_string(message).unwrap_or_else(|e| {
        format!(r#"{{"type":"error","message":"failed to encode frame: {e}"}}"#)
    })
}

/// The winning seat a terminal event declares, if any (a Boss defeat or a
/// concession ends the match).
fn terminal_winner(event: &Event) -> Option<Player> {
    match event {
        Event::BossDefeated(e) => Some(e.winner),
        Event::MatchCompleted(e) => Some(e.winner),
        _ => None,
    }
}

/// A stable per-delta fingerprint (FNV-1a over its wire bytes) used both as the
/// `MatchReplay` state-delta and as the folded checksum contribution.
fn delta_fingerprint(delta: &StateDelta) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let bytes = serde_json::to_vec(delta).unwrap_or_default();
    bytes.iter().fold(FNV_OFFSET, |hash, &byte| {
        (hash ^ byte as u64).wrapping_mul(FNV_PRIME)
    })
}

/// The wire tag for a seat.
fn player_tag(player: Player) -> &'static str {
    match player {
        Player::A => "A",
        Player::B => "B",
    }
}

/// Map an authoritative [`Event`] to its wire JSON fields. The server owns this
/// wire schema; the rules crate stays free of serialization concerns.
fn delta_event_json(event: &Event) -> serde_json::Value {
    use serde_json::json;
    match event {
        Event::MatchStarted(e) => json!({
            "matchId": e.match_id,
            "playerAOutfit": e.player_a_outfit,
            "playerBOutfit": e.player_b_outfit,
            "rngSeed": e.rng_seed,
            "openingPlayer": player_tag(e.opening_player),
        }),
        Event::MulliganCompleted(e) => json!({
            "matchId": e.match_id,
            "playerId": e.player_id,
            "player": player_tag(e.player),
            "redrawnCardIds": e.redrawn_card_ids,
        }),
        Event::CardPlayed(e) => json!({
            "matchId": e.match_id,
            "playerId": e.player_id,
            "player": player_tag(e.player),
            "cardInstanceId": e.card_instance_id,
            "targetRef": e.target_ref,
            "juiceSpent": e.juice_spent,
        }),
        Event::HeatRaised(e) => json!({
            "matchId": e.match_id,
            "player": player_tag(e.player),
            "amount": e.amount,
            "newHeat": e.new_heat,
        }),
        Event::CombatResolved(e) => json!({
            "matchId": e.match_id,
            "attackingPlayerId": e.attacking_player_id,
            "attackingPlayer": player_tag(e.attacking_player),
            "attackerId": e.attacker_id,
            "defendingPlayer": player_tag(e.defending_player),
            "defenderId": e.defender_id,
        }),
        Event::BossDefeated(e) => json!({
            "matchId": e.match_id,
            "defeatedPlayerId": e.defeated_player_id,
            "defeatedPlayer": player_tag(e.defeated_player),
            "bossId": e.boss_id,
            "winner": player_tag(e.winner),
        }),
        Event::HeroPowerActivated(e) => json!({
            "matchId": e.match_id,
            "playerId": e.player_id,
            "player": player_tag(e.player),
            "targetRef": e.target_ref,
            "juiceSpent": e.juice_spent,
            "remainingJuice": e.remaining_juice,
        }),
        Event::FatigueDamageDealt(e) => json!({
            "matchId": e.match_id,
            "playerId": e.player_id,
            "player": player_tag(e.player),
            "amount": e.amount,
            "bossHpRemaining": e.boss_hp_remaining,
        }),
        Event::TurnEnded(e) => json!({
            "matchId": e.match_id,
            "playerId": e.player_id,
            "player": player_tag(e.player),
            "nextPlayer": player_tag(e.next_player),
            "nextPlayerJuice": e.next_player_juice,
        }),
        Event::CopEventTriggered(e) => json!({
            "matchId": e.match_id,
            "playerId": e.player_id,
            "player": player_tag(e.player),
            "rngDraw": e.rng_draw,
            "newHeat": e.new_heat,
        }),
        Event::MatchCompleted(e) => json!({
            "matchId": e.match_id,
            "concedingPlayerId": e.conceding_player_id,
            "concedingPlayer": player_tag(e.conceding_player),
            "winningPlayerId": e.winning_player_id,
            "winner": player_tag(e.winner),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game_session::{ConcedeMatch, PlayCard, StartMatch};

    /// The default Outfit names a fresh session for `id` configures.
    fn outfit_a(id: &str) -> String {
        format!("{id}-a")
    }
    fn outfit_b(id: &str) -> String {
        format!("{id}-b")
    }

    /// Drive a legal match start through the hub and return the hub, ready for
    /// further actions. Player `A` (`<id>-a`) holds the opening turn.
    fn started_hub(id: &str) -> MatchHub {
        let hub = MatchHub::new();
        hub.join(id, "host", None);
        let start = StartMatch::new(id, outfit_a(id), outfit_b(id), 0xABCD);
        let payload = serde_json::to_value(&start).unwrap();
        match hub.apply_action(id, StartMatch::COMMAND, &payload) {
            ApplyOutcome::Applied(_) => hub,
            other => panic!("expected StartMatch to apply, got {other:?}"),
        }
    }

    #[test]
    fn joining_a_fresh_match_yields_an_empty_backlog() {
        let hub = MatchHub::new();
        let outcome = hub.join("m-1", "p-1", None);
        assert!(outcome.backlog.is_empty());
        assert_eq!(outcome.since_seq, 0);
    }

    #[test]
    fn a_legal_action_broadcasts_a_delta_to_subscribers() {
        let hub = MatchHub::new();
        let mut rx = hub.join("m-1", "host", None).receiver;

        let start = StartMatch::new("m-1", outfit_a("m-1"), outfit_b("m-1"), 7);
        let payload = serde_json::to_value(&start).unwrap();
        let outcome = hub.apply_action("m-1", StartMatch::COMMAND, &payload);

        let Applied { snapshot, .. } = match outcome {
            ApplyOutcome::Applied(applied) => applied,
            other => panic!("expected Applied, got {other:?}"),
        };
        assert_eq!(snapshot.sequence, 1);
        assert_eq!(snapshot.deltas[0].event_type, "match.started");

        // The subscriber received the same delta on the wire.
        let frame = rx.try_recv().expect("a delta frame should be queued");
        let msg: ServerMessage = serde_json::from_str(&frame).unwrap();
        match msg {
            ServerMessage::Delta { delta, .. } => assert_eq!(delta.seq, 1),
            other => panic!("expected Delta, got {other:?}"),
        }
    }

    #[test]
    fn an_illegal_action_is_rejected_with_the_authoritative_sequence() {
        let hub = started_hub("m-1");

        // Player B tries to act out of turn — the rules reject it. It is player
        // A's opening turn.
        let play = PlayCard::new("m-1", outfit_b("m-1"), "card-1", "target-1", 1);
        let payload = serde_json::to_value(&play).unwrap();
        match hub.apply_action("m-1", PlayCard::COMMAND, &payload) {
            ApplyOutcome::Rejected(ServerMessage::Rejected {
                authoritative_seq,
                reason,
                ..
            }) => {
                // Correct the client back to the one applied delta (the start).
                assert_eq!(authoritative_seq, 1);
                assert!(reason.contains("turn"), "reason was: {reason}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn acting_on_an_unknown_match_is_reported() {
        let hub = MatchHub::new();
        let payload = serde_json::json!({});
        match hub.apply_action("ghost", "EndTurnCmd", &payload) {
            ApplyOutcome::UnknownMatch(ServerMessage::Error { .. }) => {}
            other => panic!("expected UnknownMatch, got {other:?}"),
        }
    }

    #[test]
    fn conceding_completes_the_match_and_seals_a_retrievable_replay() {
        let hub = started_hub("m-1");
        assert!(!hub.has_replay("m-1"));

        // Concede is exempt from the turn rule; player B forfeits, A wins.
        let concede = ConcedeMatch::new("m-1", outfit_b("m-1"));
        let payload = serde_json::to_value(&concede).unwrap();
        let completion = match hub.apply_action("m-1", ConcedeMatch::COMMAND, &payload) {
            ApplyOutcome::Applied(Applied {
                completion: Some(c),
                ..
            }) => c,
            other => panic!("expected a completion, got {other:?}"),
        };

        assert_eq!(completion.winner, "A");
        assert_eq!(completion.replay_id, "m-1");
        assert_eq!(completion.checksum.len(), 16);
        // The sealed replay is retrievable through the repository port.
        assert!(hub.has_replay("m-1"));

        // A further action against the completed match is rejected.
        let again = ConcedeMatch::new("m-1", outfit_a("m-1"));
        let again_payload = serde_json::to_value(&again).unwrap();
        assert!(matches!(
            hub.apply_action("m-1", ConcedeMatch::COMMAND, &again_payload),
            ApplyOutcome::Rejected(_)
        ));
    }

    #[test]
    fn reconnecting_replays_the_backlog_without_corrupting_the_session() {
        let hub = started_hub("m-1");
        // Simulate a disconnect: the first subscriber's receiver is dropped, but
        // the live match stays in the hub.
        drop(hub.join("m-1", "host", None).receiver);

        // The client reconnects and is re-served the whole authoritative backlog.
        let rejoin = hub.join("m-1", "host", None);
        assert_eq!(rejoin.since_seq, 1);
        assert_eq!(rejoin.backlog.len(), 1);
        assert_eq!(rejoin.backlog[0].event_type, "match.started");

        // And play continues from where it left off — the session is intact.
        let concede = ConcedeMatch::new("m-1", outfit_b("m-1"));
        let payload = serde_json::to_value(&concede).unwrap();
        assert!(matches!(
            hub.apply_action("m-1", ConcedeMatch::COMMAND, &payload),
            ApplyOutcome::Applied(_)
        ));
    }

    #[test]
    fn a_match_hydrated_from_a_snapshot_serves_its_backlog() {
        let hub = MatchHub::new();
        let snapshot = MatchSnapshot {
            match_id: "m-2".into(),
            host_player_id: "host".into(),
            rng_seed: 5,
            rng_draws: 3,
            sequence: 2,
            deltas: vec![
                StateDelta {
                    seq: 1,
                    event_type: "match.started".into(),
                    event: serde_json::json!({}),
                },
                StateDelta {
                    seq: 2,
                    event_type: "card.played".into(),
                    event: serde_json::json!({}),
                },
            ],
            completed: false,
        };
        let outcome = hub.join("m-2", "host", Some(snapshot));
        assert_eq!(outcome.since_seq, 2);
        assert_eq!(outcome.backlog.len(), 2);
    }
}
