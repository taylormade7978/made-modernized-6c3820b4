//! MatchReplay bounded context — recorded match timelines for playback/analysis.
//!
//! A `MatchReplay` is the append-only, deterministic event log for a single
//! match. Replaying the log from its seed must reproduce a byte-identical final
//! `GameSession` state, so the log is governed by four invariants:
//!
//! * **Contiguous ordering** — events carry strictly monotonically increasing,
//!   gap-free sequence numbers. The next append must use exactly the current
//!   sequence; no gaps, no reorders.
//! * **Append-only immutability** — once a replay is *sealed* it can never be
//!   mutated again.
//! * **Determinism** — every appended event must carry a payload; an empty
//!   payload cannot deterministically reproduce state on replay.
//! * **Post-acknowledgement** — a reconnecting client is served only events
//!   strictly after its last acknowledged sequence number, so an append must
//!   never target a sequence at or before that acknowledged point.
//!
//! This is the first bounded context to replace its [`shared::stub_aggregate!`]
//! scaffold with hand-written command handling: `AppendEventCmd` signs and
//! appends the next match event at the current sequence number via the
//! `execute(cmd)` pattern.

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// The command name routed to [`MatchReplay::execute`] to append an event.
pub const APPEND_EVENT: &str = "AppendEventCmd";

/// Sign and append the next match event at the current sequence number.
///
/// The typed command is carried to [`MatchReplay::execute`] through the opaque
/// [`Command`] payload via [`AppendEventCmd::into_command`]; `execute` decodes
/// it back with [`AppendEventCmd::decode`]. Keeping the codec here (rather than
/// in the kernel) preserves `shared`'s zero-dependency, `wasm32`-safe contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendEventCmd {
    /// Identity of the match this event belongs to; must match the replay.
    pub match_id: String,
    /// The sequence number this event claims; must equal the replay's current
    /// (next-expected) sequence for a contiguous, gap-free log.
    pub sequence_number: u64,
    /// The opaque, deterministic event payload replayed to rebuild state.
    pub event_payload: Vec<u8>,
}

impl AppendEventCmd {
    /// Encode this command into a routable [`Command`] with an opaque payload.
    pub fn into_command(self) -> Command {
        Command::with_payload(APPEND_EVENT, self.encode())
    }

    /// Deterministic, dependency-free wire format:
    /// `[u64 sequence_number BE][u64 match_id len BE][match_id utf8][payload…]`.
    fn encode(&self) -> Vec<u8> {
        let id = self.match_id.as_bytes();
        let mut out = Vec::with_capacity(16 + id.len() + self.event_payload.len());
        out.extend_from_slice(&self.sequence_number.to_be_bytes());
        out.extend_from_slice(&(id.len() as u64).to_be_bytes());
        out.extend_from_slice(id);
        out.extend_from_slice(&self.event_payload);
        out
    }

    /// Decode a command payload produced by [`AppendEventCmd::encode`], or a
    /// [`DomainError::InvariantViolation`] if the bytes are malformed.
    fn decode(bytes: &[u8]) -> Result<Self, DomainError> {
        if bytes.len() < 16 {
            return Err(DomainError::InvariantViolation(
                "malformed AppendEventCmd payload: missing header".to_string(),
            ));
        }
        let sequence_number = u64::from_be_bytes(bytes[0..8].try_into().unwrap());
        let id_len = u64::from_be_bytes(bytes[8..16].try_into().unwrap()) as usize;
        let body = &bytes[16..];
        if body.len() < id_len {
            return Err(DomainError::InvariantViolation(
                "malformed AppendEventCmd payload: truncated match_id".to_string(),
            ));
        }
        let match_id = String::from_utf8(body[..id_len].to_vec()).map_err(|_| {
            DomainError::InvariantViolation(
                "malformed AppendEventCmd payload: match_id is not valid UTF-8".to_string(),
            )
        })?;
        let event_payload = body[id_len..].to_vec();
        Ok(Self {
            match_id,
            sequence_number,
            event_payload,
        })
    }
}

/// Domain events emitted by [`MatchReplay`].
///
/// `Clone` so `execute` can both `record` a boxed copy on the [`AggregateRoot`]
/// (for version + uncommitted-event tracking) and return the event by value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The next match event was signed and appended at `sequence_number`.
    EventAppended {
        /// Identity of the match the event was appended to.
        match_id: String,
        /// The contiguous sequence number the event was appended at.
        sequence_number: u64,
        /// The deterministic payload that will be replayed to rebuild state.
        payload: Vec<u8>,
    },
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::EventAppended { .. } => "event.appended",
        }
    }
}

/// The append-only, deterministic event log for a single match.
///
/// Embeds [`AggregateRoot`] for version and uncommitted-event tracking, and
/// adds the state needed to enforce the log's invariants: the next expected
/// sequence number, whether the log is sealed, and the last acknowledged
/// sequence number.
#[derive(Debug)]
pub struct MatchReplay {
    id: String,
    root: AggregateRoot,
    /// The sequence number the next appended event must carry. Starts at 0 and
    /// advances by exactly one per successful append, keeping the log
    /// contiguous and gap-free.
    next_sequence: u64,
    /// Once sealed the log is immutable; further appends are rejected.
    sealed: bool,
    /// The last sequence number a reconnecting client acknowledged, if any.
    /// Appends must target a sequence strictly after this point.
    last_acknowledged: Option<u64>,
}

impl MatchReplay {
    /// Create a new, empty replay log for the match identified by `id`.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            next_sequence: 0,
            sealed: false,
            last_acknowledged: None,
        }
    }

    /// This replay's match identity.
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

    /// The sequence number the next appended event must carry.
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Whether the log has been sealed (and is therefore immutable).
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// The last acknowledged sequence number, if a client has acknowledged one.
    pub fn last_acknowledged(&self) -> Option<u64> {
        self.last_acknowledged
    }

    /// Seal the log, making it append-only immutable. Idempotent.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    /// Record that a reconnecting client acknowledged up to `sequence_number`,
    /// so it is henceforth served only strictly-later events.
    pub fn acknowledge(&mut self, sequence_number: u64) {
        self.last_acknowledged = Some(match self.last_acknowledged {
            Some(prev) => prev.max(sequence_number),
            None => sequence_number,
        });
    }
}

impl Aggregate for MatchReplay {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        "MatchReplay"
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            APPEND_EVENT => {
                let cmd = AppendEventCmd::decode(&command.payload)?;

                // A valid matchId must be provided and must name this replay.
                if cmd.match_id.is_empty() || cmd.match_id != self.id {
                    return Err(DomainError::InvariantViolation(format!(
                        "AppendEventCmd match_id '{}' does not match replay '{}'",
                        cmd.match_id, self.id
                    )));
                }

                // Append-only immutability: sealed replays cannot be mutated.
                if self.sealed {
                    return Err(DomainError::InvariantViolation(format!(
                        "cannot append to sealed replay '{}'",
                        self.id
                    )));
                }

                // Contiguous, monotonically increasing ordering: no gaps/reorders.
                if cmd.sequence_number != self.next_sequence {
                    return Err(DomainError::InvariantViolation(format!(
                        "non-contiguous sequence: expected {}, got {}",
                        self.next_sequence, cmd.sequence_number
                    )));
                }

                // Post-acknowledgement: appends must target a sequence strictly
                // after any last-acknowledged point served to reconnecting clients.
                if let Some(ack) = self.last_acknowledged {
                    if cmd.sequence_number <= ack {
                        return Err(DomainError::InvariantViolation(format!(
                            "sequence {} is not strictly after last acknowledged {}",
                            cmd.sequence_number, ack
                        )));
                    }
                }

                // Determinism contract: a byte-identical replay needs a payload.
                if cmd.event_payload.is_empty() {
                    return Err(DomainError::InvariantViolation(
                        "empty event payload breaks the determinism contract".to_string(),
                    ));
                }

                let event = Event::EventAppended {
                    match_id: cmd.match_id,
                    sequence_number: cmd.sequence_number,
                    payload: cmd.event_payload,
                };
                self.root.record(Box::new(event.clone()));
                self.next_sequence += 1;
                Ok(vec![event])
            }
            // Any other command name is unknown to this aggregate.
            other => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                other.to_string(),
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

    fn cmd(match_id: &str, sequence_number: u64, payload: &[u8]) -> Command {
        AppendEventCmd {
            match_id: match_id.to_string(),
            sequence_number,
            event_payload: payload.to_vec(),
        }
        .into_command()
    }

    #[test]
    fn appends_event_and_emits_event_appended() {
        let mut replay = MatchReplay::new("m");
        let events = replay.execute(cmd("m", 0, b"seed")).expect("append accepted");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "event.appended");
        assert_eq!(replay.next_sequence(), 1);
        assert_eq!(replay.version(), 1);
        assert_eq!(replay.uncommitted_events().len(), 1);
    }

    #[test]
    fn appends_are_contiguous_across_multiple_events() {
        let mut replay = MatchReplay::new("m");
        replay.execute(cmd("m", 0, b"a")).unwrap();
        replay.execute(cmd("m", 1, b"b")).unwrap();
        assert_eq!(replay.next_sequence(), 2);
        assert_eq!(replay.version(), 2);
    }

    #[test]
    fn rejects_non_contiguous_sequence() {
        let mut replay = MatchReplay::new("m");
        // Skips sequence 0 -> gap -> rejected.
        let err = replay.execute(cmd("m", 1, b"a")).unwrap_err();
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(replay.version(), 0);
    }

    #[test]
    fn rejects_append_to_sealed_replay() {
        let mut replay = MatchReplay::new("m");
        replay.seal();
        let err = replay.execute(cmd("m", 0, b"a")).unwrap_err();
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn rejects_empty_payload_breaking_determinism() {
        let mut replay = MatchReplay::new("m");
        let err = replay.execute(cmd("m", 0, b"")).unwrap_err();
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn rejects_sequence_not_strictly_after_acknowledged() {
        let mut replay = MatchReplay::new("m");
        replay.execute(cmd("m", 0, b"a")).unwrap();
        // Client acknowledges up to sequence 1, then the log is rewound to
        // expect sequence 1 again — an append there is not strictly after the
        // acknowledged point and must be rejected.
        replay.acknowledge(1);
        replay.next_sequence = 1;
        let err = replay.execute(cmd("m", 1, b"b")).unwrap_err();
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn rejects_mismatched_match_id() {
        let mut replay = MatchReplay::new("m");
        let err = replay.execute(cmd("other", 0, b"a")).unwrap_err();
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn unknown_command_still_reported() {
        let mut replay = MatchReplay::new("m");
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
    fn command_codec_round_trips() {
        let original = AppendEventCmd {
            match_id: "match-42".to_string(),
            sequence_number: 7,
            event_payload: vec![1, 2, 3, 4],
        };
        let command = original.clone().into_command();
        assert_eq!(command.name, APPEND_EVENT);
        let decoded = AppendEventCmd::decode(&command.payload).unwrap();
        assert_eq!(decoded, original);
    }
}
