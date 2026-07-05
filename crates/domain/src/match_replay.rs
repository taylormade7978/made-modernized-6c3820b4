//! MatchReplay bounded context — recorded match timelines for playback/analysis.
//!
//! A [`MatchReplay`] is the append-only log of everything that happened in a
//! match, captured so a client can replay it deterministically or *resync*
//! after a reconnect. The log is the source of truth; four invariants keep it
//! trustworthy:
//!
//! 1. **Ordering** — events are strictly ordered by contiguous, monotonically
//!    increasing sequence numbers; no gaps or reorders are permitted.
//! 2. **Immutability** — the log is append-only and immutable once written;
//!    a *sealed* replay cannot be mutated.
//! 3. **Determinism** — replaying the log from its seed must reproduce the
//!    byte-identical final [`game_session`](../../game_session) state.
//! 4. **Reconnect contract** — a reconnecting client is served only events
//!    *strictly after* its last acknowledged sequence number.
//!
//! Two commands are implemented. [`AppendEvent`] (`AppendEventCmd`) signs and
//! appends the next match event at the current sequence number, emitting
//! [`Event::Appended`] (`event.appended`); it is the only *write* path into the
//! log and enforces all four invariants before mutating. [`RequestEventsSince`]
//! (`RequestEventsSinceCmd`) validates all four invariants and, when the
//! replay is sound, serves the tail of the log and emits [`Event::Resynced`]
//! (`replay.resynced`). This module is hand-written (it no longer uses
//! `shared::stub_aggregate!`) but preserves the same public surface — a
//! [`MatchReplay`] aggregate and a [`MatchReplayRepository`] port — so the
//! persistence adapters in `crates/mocks` keep compiling unchanged.

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, used in errors and event routing.
const AGGREGATE_TYPE: &str = "MatchReplay";

/// A single recorded entry in a replay's append-only event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedEvent {
    /// 1-based position of this event in the log. Sequence numbers are
    /// contiguous and monotonically increasing (see the ordering invariant).
    pub sequence: u64,
    /// Opaque per-event contribution to the deterministic replay digest. In a
    /// real store this would be the serialized event; here it is the byte
    /// footprint that folding must reproduce for the determinism contract.
    pub state_delta: u64,
}

/// A recorded match timeline: the append-only log plus the metadata needed to
/// verify it and to serve a reconnecting client.
#[derive(Debug)]
pub struct MatchReplay {
    id: String,
    root: AggregateRoot,
    /// Identity of the match this replay records. A `RequestEventsSinceCmd`
    /// must name this same match.
    match_id: String,
    /// The append-only, strictly-ordered event log.
    log: Vec<RecordedEvent>,
    /// Whether the log has been sealed. A sealed replay is frozen: its length
    /// at seal time is captured in `sealed_len` and must never change again.
    sealed: bool,
    /// Log length captured when the replay was sealed, used to detect mutation
    /// of an immutable (sealed) replay.
    sealed_len: usize,
    /// Digest of the seed `GameSession` state the log replays from.
    seed_digest: u64,
    /// The final-state digest that replaying the log from the seed must
    /// reproduce byte-for-byte (the determinism contract).
    expected_final_digest: u64,
}

impl MatchReplay {
    /// Create a fresh, empty replay for the match identified by `id`.
    ///
    /// A new replay is trivially sound: an empty, unsealed log whose replayed
    /// digest (just the seed) matches the expected final digest.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            match_id: id.clone(),
            id,
            root: AggregateRoot::new(),
            log: Vec::new(),
            sealed: false,
            sealed_len: 0,
            seed_digest: 0,
            expected_final_digest: 0,
        }
    }

    /// This replay's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The match this replay records.
    pub fn match_id(&self) -> &str {
        &self.match_id
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// The recorded event log, in order.
    pub fn log(&self) -> &[RecordedEvent] {
        &self.log
    }

    /// The highest sequence number present in the log (0 if empty). Serves as
    /// the reconnect high-water mark: a client cannot have acknowledged past it.
    pub fn high_water_sequence(&self) -> u64 {
        self.log.last().map(|e| e.sequence).unwrap_or(0)
    }

    /// Point the replay at a specific seed digest. The expected final digest is
    /// recomputed so the replay stays deterministic by construction.
    pub fn seed_from(&mut self, seed_digest: u64) {
        self.seed_digest = seed_digest;
        self.expected_final_digest = self.replay_digest();
    }

    /// Append an event to the log while it is still open (not sealed) and keep
    /// the expected final digest in sync so the determinism contract holds.
    ///
    /// Returns an error if the replay is already sealed — a sealed log is
    /// immutable and cannot be appended to.
    pub fn record_event(&mut self, state_delta: u64) -> Result<(), DomainError> {
        if self.sealed {
            return Err(DomainError::InvariantViolation(format!(
                "replay '{}' is sealed and cannot be appended to",
                self.id
            )));
        }
        let sequence = self.high_water_sequence() + 1;
        self.log.push(RecordedEvent {
            sequence,
            state_delta,
        });
        self.expected_final_digest = self.replay_digest();
        Ok(())
    }

    /// Seal the replay, freezing its current length. After sealing the log is
    /// immutable: any change to its length is a mutation of a sealed replay.
    pub fn seal(&mut self) {
        self.sealed = true;
        self.sealed_len = self.log.len();
    }

    /// Fold the seed digest over the log to reproduce the final-state digest.
    /// Determinism holds when this equals [`Self::expected_final_digest`].
    fn replay_digest(&self) -> u64 {
        self.log
            .iter()
            .fold(self.seed_digest, |acc, e| acc.wrapping_add(e.state_delta))
    }

    /// Ordering invariant: sequence numbers are `1, 2, …, N` with no gaps or
    /// reorders.
    fn ensure_ordered(&self) -> Result<(), DomainError> {
        for (index, event) in self.log.iter().enumerate() {
            let expected = index as u64 + 1;
            if event.sequence != expected {
                return Err(DomainError::InvariantViolation(format!(
                    "replay log is not contiguously ordered: expected sequence {expected} at \
                     position {index}, found {}",
                    event.sequence
                )));
            }
        }
        Ok(())
    }

    /// Immutability invariant: a sealed replay must still have exactly the
    /// length it was sealed at.
    fn ensure_seal_intact(&self) -> Result<(), DomainError> {
        if self.sealed && self.log.len() != self.sealed_len {
            return Err(DomainError::InvariantViolation(format!(
                "sealed replay '{}' was mutated: sealed at {} events, now {}",
                self.id,
                self.sealed_len,
                self.log.len()
            )));
        }
        Ok(())
    }

    /// Determinism invariant: replaying the log from the seed reproduces the
    /// expected byte-identical final state.
    fn ensure_deterministic(&self) -> Result<(), DomainError> {
        let actual = self.replay_digest();
        if actual != self.expected_final_digest {
            return Err(DomainError::InvariantViolation(format!(
                "replay is non-deterministic: replaying from seed yields {actual:#x}, expected \
                 {:#x}",
                self.expected_final_digest
            )));
        }
        Ok(())
    }

    /// Handle `AppendEventCmd`: sign and append the next match event at the
    /// current sequence number, emitting [`Event::Appended`].
    ///
    /// This is the only write path into the log, so it enforces every invariant
    /// before mutating: the existing log must already be sound (ordered, its
    /// seal intact, deterministic), the replay must be open (an immutable sealed
    /// replay cannot be appended to), and the signed sequence number must sit
    /// strictly after the high-water mark *and* be exactly the contiguous next
    /// sequence — no gaps, reorders, or replays of already-served events.
    fn append_event(&mut self, request: AppendEvent) -> Result<Vec<Event>, DomainError> {
        // The command must name the match this replay actually records.
        if request.match_id != self.match_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets match '{}' but this replay records '{}'",
                request.match_id, self.match_id
            )));
        }

        // The existing log must be sound before we extend it.
        self.ensure_ordered()?;
        self.ensure_seal_intact()?;
        self.ensure_deterministic()?;

        // Immutability: a sealed replay is frozen and can never be appended to.
        if self.sealed {
            return Err(DomainError::InvariantViolation(format!(
                "replay '{}' is sealed and cannot be appended to",
                self.id
            )));
        }

        let high_water = self.high_water_sequence();

        // Reconnect contract: only an event strictly after the high-water mark
        // may be signed — re-signing an already-recorded sequence is a reorder.
        if request.sequence_number <= high_water {
            return Err(DomainError::InvariantViolation(format!(
                "append sequence {} is not strictly after the high-water mark {}",
                request.sequence_number, high_water
            )));
        }

        // Ordering: the signed sequence must be exactly the contiguous next
        // sequence, leaving no gap in the strictly-increasing log.
        let expected = high_water + 1;
        if request.sequence_number != expected {
            return Err(DomainError::InvariantViolation(format!(
                "append sequence {} is not contiguous: expected the next sequence {expected}",
                request.sequence_number
            )));
        }

        // Sign and append, keeping the determinism digest in sync so the log
        // still replays byte-for-byte to its expected final state.
        self.log.push(RecordedEvent {
            sequence: expected,
            state_delta: request.event_payload,
        });
        self.expected_final_digest = self.replay_digest();

        let event = Event::Appended {
            match_id: self.match_id.clone(),
            sequence: expected,
        };
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `RequestEventsSinceCmd`: verify the replay is sound, then serve
    /// every event strictly after the client's last acknowledged sequence and
    /// emit [`Event::Resynced`].
    fn request_events_since(
        &mut self,
        request: RequestEventsSince,
    ) -> Result<Vec<Event>, DomainError> {
        // The command must name the match this replay actually records.
        if request.match_id != self.match_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets match '{}' but this replay records '{}'",
                request.match_id, self.match_id
            )));
        }

        // Enforce the standing invariants of the log before serving anything.
        self.ensure_ordered()?;
        self.ensure_seal_intact()?;
        self.ensure_deterministic()?;

        // Reconnect contract: a client cannot acknowledge past what the replay
        // has produced, and it is served only events strictly after that point.
        let high_water = self.high_water_sequence();
        if request.last_acked_sequence > high_water {
            return Err(DomainError::InvariantViolation(format!(
                "last acknowledged sequence {} is beyond the replay high-water mark {}",
                request.last_acked_sequence, high_water
            )));
        }

        let served: Vec<u64> = self
            .log
            .iter()
            .map(|e| e.sequence)
            .filter(|seq| *seq > request.last_acked_sequence)
            .collect();

        let event = Event::Resynced {
            match_id: self.match_id.clone(),
            since_sequence: request.last_acked_sequence,
            served_sequences: served,
        };
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

/// Typed form of the `AppendEventCmd` command.
///
/// A recorder signs the next match event into the log at the current sequence
/// number. Like [`RequestEventsSince`], it owns its own wire encoding because
/// the [`shared`] kernel carries command payloads as opaque bytes. The encoding
/// is `"<matchId>:<sequenceNumber>:<eventPayload>"`; the two numeric fields are
/// parsed from the right so a match id may itself contain colons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendEvent {
    /// The match whose log is being appended to.
    pub match_id: String,
    /// The sequence number being signed; must be the contiguous next sequence.
    pub sequence_number: u64,
    /// The event's opaque contribution to the deterministic replay digest
    /// (its `state_delta` in the recorded log).
    pub event_payload: u64,
}

impl AppendEvent {
    /// The command name this maps to.
    pub const COMMAND: &'static str = "AppendEventCmd";

    /// Build an append of `event_payload` at `sequence_number` for `match_id`.
    pub fn new(match_id: impl Into<String>, sequence_number: u64, event_payload: u64) -> Self {
        Self {
            match_id: match_id.into(),
            sequence_number,
            event_payload,
        }
    }

    /// Encode this append as a dispatchable [`Command`].
    pub fn into_command(self) -> Command {
        let payload = format!(
            "{}:{}:{}",
            self.match_id, self.sequence_number, self.event_payload
        )
        .into_bytes();
        Command::with_payload(Self::COMMAND, payload)
    }

    /// Decode a command payload of the form
    /// `"<matchId>:<sequenceNumber>:<eventPayload>"`.
    fn decode(payload: &[u8]) -> Result<Self, DomainError> {
        let text = std::str::from_utf8(payload).map_err(|_| {
            DomainError::InvariantViolation("AppendEventCmd payload is not UTF-8".to_string())
        })?;
        // Peel the two numeric fields off the right so a match id may itself
        // contain colons.
        let (rest, payload_str) = text.rsplit_once(':').ok_or_else(|| {
            DomainError::InvariantViolation(
                "AppendEventCmd payload must be '<matchId>:<sequenceNumber>:<eventPayload>'"
                    .to_string(),
            )
        })?;
        let (match_id, seq_str) = rest.rsplit_once(':').ok_or_else(|| {
            DomainError::InvariantViolation(
                "AppendEventCmd payload must be '<matchId>:<sequenceNumber>:<eventPayload>'"
                    .to_string(),
            )
        })?;
        let sequence_number = seq_str.parse::<u64>().map_err(|_| {
            DomainError::InvariantViolation(format!(
                "AppendEventCmd sequence number '{seq_str}' is not a valid number"
            ))
        })?;
        let event_payload = payload_str.parse::<u64>().map_err(|_| {
            DomainError::InvariantViolation(format!(
                "AppendEventCmd event payload '{payload_str}' is not a valid number"
            ))
        })?;
        if match_id.is_empty() {
            return Err(DomainError::InvariantViolation(
                "AppendEventCmd requires a non-empty matchId".to_string(),
            ));
        }
        Ok(Self {
            match_id: match_id.to_string(),
            sequence_number,
            event_payload,
        })
    }
}

/// Typed form of the `RequestEventsSinceCmd` command.
///
/// A reconnecting client asks the replay for everything it has missed since the
/// last sequence number it acknowledged. Because the [`shared`] kernel carries
/// commands as an opaque byte payload (no serde dependency, for `wasm32`), this
/// type also owns the trivial `"<matchId>:<lastAckedSequence>"` wire encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestEventsSince {
    /// The match the client is resyncing.
    pub match_id: String,
    /// The last sequence number the client has already acknowledged; it will be
    /// served only events strictly greater than this.
    pub last_acked_sequence: u64,
}

impl RequestEventsSince {
    /// The command name this maps to.
    pub const COMMAND: &'static str = "RequestEventsSinceCmd";

    /// Build a request for `match_id` resuming after `last_acked_sequence`.
    pub fn new(match_id: impl Into<String>, last_acked_sequence: u64) -> Self {
        Self {
            match_id: match_id.into(),
            last_acked_sequence,
        }
    }

    /// Encode this request as a dispatchable [`Command`].
    pub fn into_command(self) -> Command {
        let payload = format!("{}:{}", self.match_id, self.last_acked_sequence).into_bytes();
        Command::with_payload(Self::COMMAND, payload)
    }

    /// Decode a command payload of the form `"<matchId>:<lastAckedSequence>"`.
    fn decode(payload: &[u8]) -> Result<Self, DomainError> {
        let text = std::str::from_utf8(payload).map_err(|_| {
            DomainError::InvariantViolation(
                "RequestEventsSinceCmd payload is not UTF-8".to_string(),
            )
        })?;
        // Split on the final ':' so a match id may itself contain colons.
        let (match_id, seq) = text.rsplit_once(':').ok_or_else(|| {
            DomainError::InvariantViolation(
                "RequestEventsSinceCmd payload must be '<matchId>:<lastAckedSequence>'".to_string(),
            )
        })?;
        let last_acked_sequence = seq.parse::<u64>().map_err(|_| {
            DomainError::InvariantViolation(format!(
                "RequestEventsSinceCmd last acknowledged sequence '{seq}' is not a valid number"
            ))
        })?;
        if match_id.is_empty() {
            return Err(DomainError::InvariantViolation(
                "RequestEventsSinceCmd requires a non-empty matchId".to_string(),
            ));
        }
        Ok(Self {
            match_id: match_id.to_string(),
            last_acked_sequence,
        })
    }
}

/// Domain events emitted by [`MatchReplay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The next match event was signed and appended to the log: it names the
    /// match and the sequence number the event was recorded at.
    Appended {
        /// The match whose log grew.
        match_id: String,
        /// The sequence number assigned to the appended event.
        sequence: u64,
    },
    /// The replay was resynced for a reconnecting client: it names the match,
    /// the sequence the client resumed after, and the sequence numbers served
    /// (all strictly greater than `since_sequence`).
    Resynced {
        /// The match that was resynced.
        match_id: String,
        /// The last sequence the client had acknowledged.
        since_sequence: u64,
        /// The sequence numbers served, strictly after `since_sequence`.
        served_sequences: Vec<u64>,
    },
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::Appended { .. } => "event.appended",
            Event::Resynced { .. } => "replay.resynced",
        }
    }
}

impl Aggregate for MatchReplay {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            AppendEvent::COMMAND => {
                let request = AppendEvent::decode(&command.payload)?;
                self.append_event(request)
            }
            RequestEventsSince::COMMAND => {
                let request = RequestEventsSince::decode(&command.payload)?;
                self.request_events_since(request)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`MatchReplay`] aggregate. Adapters implement
/// [`Repository`] for [`MatchReplay`] and then this marker trait.
pub trait MatchReplayRepository: Repository<MatchReplay> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sound, sealed replay of three contiguous events for match `m-1`.
    fn valid_replay() -> MatchReplay {
        let mut replay = open_replay();
        replay.seal();
        replay
    }

    /// A sound, *open* (unsealed) replay of three contiguous events for match
    /// `m-1` — ready to accept an `AppendEventCmd` at sequence 4.
    fn open_replay() -> MatchReplay {
        let mut replay = MatchReplay::new("m-1");
        replay.seed_from(7);
        replay.record_event(10).unwrap();
        replay.record_event(20).unwrap();
        replay.record_event(30).unwrap();
        replay
    }

    // Scenario: Successfully execute AppendEventCmd.
    #[test]
    fn appends_and_emits_event_appended_event() {
        let mut replay = open_replay();

        let events = replay
            .execute(AppendEvent::new("m-1", 4, 40).into_command())
            .expect("valid append should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "event.appended");
        match &events[0] {
            Event::Appended { match_id, sequence } => {
                assert_eq!(match_id, "m-1");
                assert_eq!(*sequence, 4);
            }
            other => panic!("expected Appended, got {other:?}"),
        }
        // The event was appended to the log and recorded on the aggregate root.
        assert_eq!(replay.high_water_sequence(), 4);
        assert_eq!(replay.log().len(), 4);
        assert_eq!(replay.uncommitted_events().len(), 1);
        assert_eq!(replay.version(), 1);
    }

    // Scenario: rejected — events must be contiguous, monotonically increasing;
    // no gaps or reorders.
    #[test]
    fn append_rejected_when_log_is_not_contiguously_ordered() {
        let mut replay = open_replay();
        // Corrupt the existing log so it is no longer contiguously ordered.
        replay.log[1].sequence = 5;

        let err = replay
            .execute(AppendEvent::new("m-1", 4, 40).into_command())
            .expect_err("append onto a non-contiguous log must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: rejected — a gap in the signed sequence is a reorder/gap.
    #[test]
    fn append_rejected_when_sequence_leaves_a_gap() {
        let mut replay = open_replay();
        // The next contiguous sequence is 4; signing 6 would leave a gap.
        let err = replay
            .execute(AppendEvent::new("m-1", 6, 40).into_command())
            .expect_err("a gap in the signed sequence must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: rejected — the log is append-only and immutable once written;
    // sealed replays cannot be mutated.
    #[test]
    fn append_rejected_when_replay_is_sealed() {
        let mut replay = valid_replay(); // sealed at three events

        let err = replay
            .execute(AppendEvent::new("m-1", 4, 40).into_command())
            .expect_err("appending to a sealed replay must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        // The sealed log was not mutated.
        assert_eq!(replay.log().len(), 3);
    }

    // Scenario: rejected — replaying the log from its seed must reproduce
    // byte-identical final GameSession state (determinism contract).
    #[test]
    fn append_rejected_when_replay_is_non_deterministic() {
        let mut replay = open_replay();
        // Corrupt the expected final digest so replaying no longer reproduces it.
        replay.expected_final_digest = replay.expected_final_digest.wrapping_add(1);

        let err = replay
            .execute(AppendEvent::new("m-1", 4, 40).into_command())
            .expect_err("append onto a non-deterministic replay must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: rejected — a reconnecting client is served only events strictly
    // after its last acknowledged sequence number (no re-signing a recorded one).
    #[test]
    fn append_rejected_when_sequence_is_not_strictly_after_high_water() {
        let mut replay = open_replay();
        // Sequence 3 is already recorded; re-signing it is not strictly after
        // the high-water mark of 3.
        let err = replay
            .execute(AppendEvent::new("m-1", 3, 40).into_command())
            .expect_err("re-signing a recorded sequence must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // The command must name the match this replay actually records.
    #[test]
    fn append_rejected_when_match_id_mismatches() {
        let mut replay = open_replay();

        let err = replay
            .execute(AppendEvent::new("other-match", 4, 40).into_command())
            .expect_err("append targeting a different match must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn append_command_payload_round_trips() {
        // A match id containing a colon still round-trips: the two numeric
        // fields are parsed off the right.
        let command = AppendEvent::new("m:42", 7, 99).into_command();
        assert_eq!(command.name, AppendEvent::COMMAND);
        let decoded = AppendEvent::decode(&command.payload).unwrap();
        assert_eq!(decoded, AppendEvent::new("m:42", 7, 99));
    }

    // Scenario: Successfully execute RequestEventsSinceCmd.
    #[test]
    fn resyncs_and_emits_replay_resynced_event() {
        let mut replay = valid_replay();

        let events = replay
            .execute(RequestEventsSince::new("m-1", 1).into_command())
            .expect("valid resync should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "replay.resynced");
        match &events[0] {
            Event::Resynced {
                match_id,
                since_sequence,
                served_sequences,
            } => {
                assert_eq!(match_id, "m-1");
                assert_eq!(*since_sequence, 1);
                // Only events strictly after sequence 1 are served.
                assert_eq!(served_sequences, &[2, 3]);
            }
            other => panic!("expected Resynced, got {other:?}"),
        }
        // The event was recorded on the aggregate root.
        assert_eq!(replay.uncommitted_events().len(), 1);
        assert_eq!(replay.version(), 1);
    }

    // Scenario: rejected — events must be contiguous, monotonically increasing;
    // no gaps or reorders.
    #[test]
    fn rejects_when_log_has_a_gap() {
        let mut replay = valid_replay();
        // Introduce a gap: rewrite the middle sequence so it is no longer
        // contiguous (…1, 5, 3… breaks ordering).
        replay.log[1].sequence = 5;

        let err = replay
            .execute(RequestEventsSince::new("m-1", 0).into_command())
            .expect_err("non-contiguous log must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: rejected — the log is append-only and immutable once written;
    // sealed replays cannot be mutated.
    #[test]
    fn rejects_when_sealed_replay_was_mutated() {
        let mut replay = valid_replay();
        // Mutate a sealed replay by appending directly, bypassing the seal.
        replay.log.push(RecordedEvent {
            sequence: 4,
            state_delta: 40,
        });
        replay.expected_final_digest = replay.replay_digest();

        let err = replay
            .execute(RequestEventsSince::new("m-1", 0).into_command())
            .expect_err("mutated sealed replay must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: rejected — replaying the log from its seed must reproduce
    // byte-identical final GameSession state (determinism contract).
    #[test]
    fn rejects_when_replay_is_non_deterministic() {
        let mut replay = valid_replay();
        // Corrupt the expected final digest so replaying no longer reproduces it.
        replay.expected_final_digest = replay.expected_final_digest.wrapping_add(1);

        let err = replay
            .execute(RequestEventsSince::new("m-1", 0).into_command())
            .expect_err("non-deterministic replay must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: rejected — a reconnecting client is served only events strictly
    // after its last acknowledged sequence number.
    #[test]
    fn rejects_when_last_acked_is_beyond_high_water_mark() {
        let mut replay = valid_replay();

        // The client claims to have acknowledged sequence 9, but the replay
        // only reaches 3 — there is nothing strictly after to serve honestly.
        let err = replay
            .execute(RequestEventsSince::new("m-1", 9).into_command())
            .expect_err("ack past the high-water mark must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut replay = MatchReplay::new("m-1");
        let err = replay.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "MatchReplay");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let command = RequestEventsSince::new("m-42", 7).into_command();
        assert_eq!(command.name, RequestEventsSince::COMMAND);
        let decoded = RequestEventsSince::decode(&command.payload).unwrap();
        assert_eq!(decoded, RequestEventsSince::new("m-42", 7));
    }
}
