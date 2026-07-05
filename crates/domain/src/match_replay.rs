//! MatchReplay bounded context — recorded match timelines for playback/analysis.
//!
//! A `MatchReplay` is the append-only, deterministic record of a completed
//! match: an ordered log of frames replayed from a fixed `seed`. Once a match
//! ends it is *sealed* via [`SealReplayCmd`], which freezes the log and emits a
//! `replay.sealed` event. Sealing is the write entrypoint that enforces the
//! four MatchReplay invariants:
//!
//! 1. **Contiguous ordering** — frames carry strictly increasing, gap-free
//!    sequence numbers (`0, 1, 2, …`); a gap or reorder is rejected.
//! 2. **Append-only immutability** — a replay may be sealed exactly once; a
//!    sealed replay cannot be mutated or re-sealed.
//! 3. **Determinism** — replaying the log from its seed must reproduce the
//!    `final_state_hash` the caller claims; a mismatch is rejected.
//! 4. **Reconnect safety** — a client's last-acknowledged sequence must fall
//!    within the recorded log, so reconnects are served only real events
//!    strictly after that point.
//!
//! This mirrors the `shared::stub_aggregate!` contract (see [`shared`]) but
//! replaces the generated stub with hand-written command/event handling, using
//! the decide→apply split: [`MatchReplay::execute`] validates and produces the
//! event, [`MatchReplay::apply`] is the sole state mutator (so it can be reused
//! to rehydrate an aggregate from its event log).

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// One frame of the recorded match timeline.
///
/// A frame pairs its position in the log (`sequence`) with the deterministic
/// `input` applied to the GameSession at that step. The final GameSession state
/// hash is a pure fold of the seed over every frame's `(sequence, input)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayFrame {
    /// Zero-based position in the append-only log. Must be contiguous.
    pub sequence: u64,
    /// The deterministic input applied to the GameSession at this frame.
    pub input: u64,
}

impl ReplayFrame {
    /// Build a frame at `sequence` carrying `input`.
    pub fn new(sequence: u64, input: u64) -> Self {
        Self { sequence, input }
    }
}

/// Typed `SealReplayCmd`: finalize and freeze a completed match's replay.
///
/// Carried over the wire as an opaque [`Command`] (the [`shared`] kernel is
/// serialization-agnostic and dependency-free), so this type owns its own
/// compact encode/decode into `Command::payload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealReplayCmd {
    /// Identity of the match being sealed; must equal the aggregate's id.
    pub match_id: String,
    /// The claimed final GameSession state hash the sealed log must reproduce.
    pub final_state_hash: u64,
}

impl SealReplayCmd {
    /// The routing name matched by [`MatchReplay::execute`].
    pub const NAME: &'static str = "SealReplayCmd";

    /// Encode into the transport [`Command`] as `"{match_id}\n{final_state_hash}"`.
    pub fn into_command(self) -> Command {
        let payload = format!("{}\n{}", self.match_id, self.final_state_hash).into_bytes();
        Command::with_payload(Self::NAME, payload)
    }

    /// Decode from a [`Command`] payload, rejecting malformed input.
    fn decode(payload: &[u8]) -> Result<Self, DomainError> {
        let text = std::str::from_utf8(payload).map_err(|_| {
            DomainError::InvariantViolation("SealReplayCmd payload is not UTF-8".into())
        })?;
        let (match_id, hash) = text.split_once('\n').ok_or_else(|| {
            DomainError::InvariantViolation("SealReplayCmd payload missing final_state_hash".into())
        })?;
        let final_state_hash = hash.parse::<u64>().map_err(|_| {
            DomainError::InvariantViolation(
                "SealReplayCmd final_state_hash is not a valid hash".into(),
            )
        })?;
        if match_id.is_empty() {
            return Err(DomainError::InvariantViolation(
                "SealReplayCmd match_id is required".into(),
            ));
        }
        Ok(Self {
            match_id: match_id.to_string(),
            final_state_hash,
        })
    }
}

/// Domain events emitted by [`MatchReplay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The replay was finalized and frozen; the log is now immutable.
    ReplaySealed {
        /// The sealed match's identity.
        match_id: String,
        /// The deterministic final GameSession state hash the log reproduces.
        final_state_hash: u64,
        /// Number of frames frozen into the sealed log.
        frame_count: u64,
    },
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::ReplaySealed { .. } => "replay.sealed",
        }
    }
}

/// The MatchReplay aggregate: an append-only, deterministic match timeline.
#[derive(Debug)]
pub struct MatchReplay {
    id: String,
    root: AggregateRoot,
    /// Deterministic seed the log replays from.
    seed: u64,
    /// The append-only frame log, in recorded order.
    log: Vec<ReplayFrame>,
    /// Highest sequence a reconnecting client acknowledged, if any.
    last_acknowledged_seq: Option<u64>,
    /// Whether the replay has been sealed (frozen and immutable).
    sealed: bool,
}

impl MatchReplay {
    /// Create a new, empty replay with the given identity and seed `0`.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            seed: 0,
            log: Vec::new(),
            last_acknowledged_seq: None,
            sealed: false,
        }
    }

    /// This replay's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// The deterministic seed this replay's log folds from.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Set the deterministic seed. Rejected once the replay is sealed.
    pub fn set_seed(&mut self, seed: u64) -> Result<(), DomainError> {
        self.ensure_mutable()?;
        self.seed = seed;
        Ok(())
    }

    /// The recorded frame log, in order.
    pub fn log(&self) -> &[ReplayFrame] {
        &self.log
    }

    /// The last sequence a reconnecting client acknowledged, if any.
    pub fn last_acknowledged_seq(&self) -> Option<u64> {
        self.last_acknowledged_seq
    }

    /// Whether the replay has been sealed.
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Append a frame with the next contiguous sequence and return its number.
    ///
    /// This is the safe recording path: it can never introduce a gap or reorder
    /// (invariant 1) and refuses to mutate a sealed log (invariant 2).
    pub fn record_frame(&mut self, input: u64) -> Result<u64, DomainError> {
        self.ensure_mutable()?;
        let sequence = self.log.len() as u64;
        self.log.push(ReplayFrame::new(sequence, input));
        Ok(sequence)
    }

    /// Append a frame with an explicit sequence, without enforcing contiguity.
    ///
    /// For rehydrating a persisted/replicated log — or, in tests, simulating a
    /// tampered/out-of-order log whose corruption is caught at seal time. Still
    /// refuses to mutate a sealed replay (invariant 2).
    pub fn append_raw(&mut self, frame: ReplayFrame) -> Result<(), DomainError> {
        self.ensure_mutable()?;
        self.log.push(frame);
        Ok(())
    }

    /// Record that a reconnecting client acknowledged up to `sequence`.
    pub fn acknowledge(&mut self, sequence: u64) -> Result<(), DomainError> {
        self.ensure_mutable()?;
        self.last_acknowledged_seq = Some(sequence);
        Ok(())
    }

    /// The deterministic final GameSession state hash produced by replaying the
    /// current log from the seed. A pure fold — same seed + same frames always
    /// yield the same hash, which is the determinism contract callers seal on.
    pub fn computed_final_hash(&self) -> u64 {
        Self::replay_hash(self.seed, &self.log)
    }

    /// Pure fold of `seed` over every frame's `(sequence, input)`. FNV-1a-style
    /// mixing keeps it dependency-free and byte-stable across native and WASM.
    fn replay_hash(seed: u64, log: &[ReplayFrame]) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut acc = seed ^ FNV_OFFSET;
        for frame in log {
            acc = (acc ^ frame.sequence).wrapping_mul(FNV_PRIME);
            acc = (acc ^ frame.input).wrapping_mul(FNV_PRIME);
        }
        acc
    }

    /// Guard shared by every mutator: a sealed replay is immutable (invariant 2).
    fn ensure_mutable(&self) -> Result<(), DomainError> {
        if self.sealed {
            return Err(DomainError::InvariantViolation(
                "replay is sealed: the log is append-only and immutable once written".into(),
            ));
        }
        Ok(())
    }

    /// Validate the seal request against every invariant and produce the event.
    /// Pure — mutates nothing (that is [`MatchReplay::apply`]'s job).
    fn decide_seal(&self, cmd: &SealReplayCmd) -> Result<Event, DomainError> {
        // Invariant 2: a sealed replay cannot be mutated or re-sealed.
        if self.sealed {
            return Err(DomainError::InvariantViolation(
                "replay is already sealed: sealed replays cannot be mutated".into(),
            ));
        }

        // A valid matchId must be provided and identify this aggregate.
        if cmd.match_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "matchId '{}' does not match replay '{}'",
                cmd.match_id, self.id
            )));
        }

        // Invariant 1: sequences must be contiguous, gap-free, and in order.
        for (index, frame) in self.log.iter().enumerate() {
            if frame.sequence != index as u64 {
                return Err(DomainError::InvariantViolation(format!(
                    "event log is not strictly ordered: frame at position {index} has sequence {}",
                    frame.sequence
                )));
            }
        }

        // Invariant 4: a client cannot have acknowledged past the recorded log,
        // or reconnects would be served phantom events after that sequence.
        if let Some(ack) = self.last_acknowledged_seq {
            let last_sequence = self.log.last().map(|f| f.sequence);
            let within = last_sequence.is_some_and(|last| ack <= last);
            if !within {
                return Err(DomainError::InvariantViolation(format!(
                    "last acknowledged sequence {ack} is beyond the recorded log"
                )));
            }
        }

        // Invariant 3: replaying from the seed must reproduce the claimed hash.
        let replayed = self.computed_final_hash();
        if replayed != cmd.final_state_hash {
            return Err(DomainError::InvariantViolation(format!(
                "determinism violation: replay from seed yields {replayed}, but finalStateHash is {}",
                cmd.final_state_hash
            )));
        }

        Ok(Event::ReplaySealed {
            match_id: cmd.match_id.clone(),
            final_state_hash: cmd.final_state_hash,
            frame_count: self.log.len() as u64,
        })
    }

    /// The sole state mutator. Applying an event advances aggregate state and is
    /// reusable to rehydrate the aggregate by folding its event log on load.
    fn apply(&mut self, event: &Event) {
        match event {
            Event::ReplaySealed { .. } => self.sealed = true,
        }
    }
}

impl Aggregate for MatchReplay {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        "MatchReplay"
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            SealReplayCmd::NAME => {
                let cmd = SealReplayCmd::decode(&command.payload)?;
                let event = self.decide_seal(&cmd)?;
                self.apply(&event);
                self.root.record(Box::new(event.clone()));
                Ok(vec![event])
            }
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`MatchReplay`] aggregate. Adapters implement
/// [`Repository`] for `MatchReplay` and then this marker trait.
pub trait MatchReplayRepository: Repository<MatchReplay> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a valid, contiguous, unsealed replay whose hash callers can seal on.
    fn valid_replay() -> MatchReplay {
        let mut replay = MatchReplay::new("match-42");
        replay.set_seed(7).unwrap();
        replay.record_frame(11).unwrap();
        replay.record_frame(22).unwrap();
        replay.record_frame(33).unwrap();
        replay
    }

    fn seal(match_id: &str, final_state_hash: u64) -> Command {
        SealReplayCmd {
            match_id: match_id.to_string(),
            final_state_hash,
        }
        .into_command()
    }

    #[test]
    fn seal_replay_emits_replay_sealed_event() {
        let mut replay = valid_replay();
        let hash = replay.computed_final_hash();

        let events = replay
            .execute(seal("match-42", hash))
            .expect("seal should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "replay.sealed");
        assert!(matches!(
            &events[0],
            Event::ReplaySealed { match_id, final_state_hash, frame_count }
                if match_id == "match-42" && *final_state_hash == hash && *frame_count == 3
        ));
        assert!(replay.is_sealed());
        assert_eq!(replay.version(), 1);
        assert_eq!(replay.uncommitted_events().len(), 1);
    }

    #[test]
    fn rejects_non_contiguous_sequence() {
        let mut replay = MatchReplay::new("match-42");
        replay.append_raw(ReplayFrame::new(0, 11)).unwrap();
        // Gap: sequence 2 where 1 is expected — a reorder/gap.
        replay.append_raw(ReplayFrame::new(2, 22)).unwrap();
        let hash = replay.computed_final_hash();

        let err = replay.execute(seal("match-42", hash)).unwrap_err();
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert!(!replay.is_sealed());
    }

    #[test]
    fn rejects_mutation_of_sealed_replay() {
        let mut replay = valid_replay();
        let hash = replay.computed_final_hash();
        replay
            .execute(seal("match-42", hash))
            .expect("first seal succeeds");

        // Re-sealing a frozen replay is a mutation and must be rejected.
        let err = replay.execute(seal("match-42", hash)).unwrap_err();
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        // And the append-only guard blocks further recording.
        assert!(replay.record_frame(99).is_err());
    }

    #[test]
    fn rejects_determinism_mismatch() {
        let mut replay = valid_replay();
        let wrong_hash = replay.computed_final_hash().wrapping_add(1);

        let err = replay.execute(seal("match-42", wrong_hash)).unwrap_err();
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert!(!replay.is_sealed());
    }

    #[test]
    fn rejects_acknowledgement_beyond_log() {
        let mut replay = valid_replay();
        // Client claims to have acked sequence 9, but the log ends at 2.
        replay.acknowledge(9).unwrap();
        let hash = replay.computed_final_hash();

        let err = replay.execute(seal("match-42", hash)).unwrap_err();
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert!(!replay.is_sealed());
    }

    #[test]
    fn rejects_unknown_command() {
        let mut replay = MatchReplay::new("match-42");
        let err = replay.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "MatchReplay");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }
}
