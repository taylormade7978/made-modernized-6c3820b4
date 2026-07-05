//! MatchmakingTicket bounded context — a player's request to be matched into a game.
//!
//! A [`MatchmakingTicket`] is one player's standing request to be paired into a
//! game. While it sits in the queue the matchmaker widens its search over time;
//! if no opponent is found before the cap, the ticket falls back to an
//! exhibition game. Four invariants govern a ticket:
//!
//! 1. **Search bands** — primary targeting is ±150 Rating and ±5 Level, and the
//!    bands expand *monotonically* as the ticket ages (they never shrink below
//!    the primary width).
//! 2. **Fallback cap** — a ticket must fall back to exhibition only after
//!    5 minutes ([`FALLBACK_CAP_SECONDS`]) of unmatched queueing.
//! 3. **Pairing** — a ticket may be paired with exactly one opponent and never
//!    with the ticket's own player.
//! 4. **Terminal state** — a cancelled or matched ticket cannot be re-matched.
//!
//! Two commands are implemented. [`EnqueueTicket`] (`EnqueueTicketCmd`) places a
//! player into the ranked queue with the initial primary search bands and, on
//! success, emits [`Event::TicketEnqueued`] (`ticket.enqueued`). [`FallbackToExhibition`]
//! (`FallbackToExhibitionCmd`) routes an unmatched, still-queued ticket to an
//! exhibition game once the cap has elapsed, and on success emits
//! [`Event::FellBackToExhibition`] (`ticket.fell.back.to.exhibition`). Both
//! enforce every invariant above before recording anything.
//! This module is hand-written (it no longer uses `shared::stub_aggregate!`) but
//! preserves the same public surface — a [`MatchmakingTicket`] aggregate and a
//! [`MatchmakingTicketRepository`] port — so the persistence adapters in
//! `crates/mocks` keep compiling unchanged.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "MatchmakingTicket";

/// The command name that places a player into the ranked queue.
const ENQUEUE_TICKET: &str = "EnqueueTicketCmd";

/// The command name that routes an unmatched ticket to an exhibition game.
const FALLBACK_TO_EXHIBITION: &str = "FallbackToExhibitionCmd";

/// Primary Rating search band: matchmaking initially targets opponents within
/// ±150 Rating. As the ticket ages the band may only widen, never shrink below
/// this primary width.
pub const PRIMARY_RATING_BAND: u32 = 150;

/// Primary Level search band: matchmaking initially targets opponents within
/// ±5 Level, expanding monotonically thereafter.
pub const PRIMARY_LEVEL_BAND: u32 = 5;

/// The unmatched-queueing cap, in seconds: a ticket falls back to exhibition
/// only after 5 minutes of going unmatched.
pub const FALLBACK_CAP_SECONDS: u64 = 5 * 60;

/// Lifecycle state of a matchmaking ticket. A ticket is re-matchable only while
/// [`Queued`](TicketStatus::Queued); the other states are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketStatus {
    /// Actively searching for an opponent; the only re-matchable state.
    Queued,
    /// Already paired with an opponent — a matched ticket cannot be re-matched.
    Matched,
    /// Withdrawn from the queue — a cancelled ticket cannot be re-matched.
    Cancelled,
    /// Routed to an exhibition game after exceeding the fallback cap.
    FellBackToExhibition,
}

/// The `EnqueueTicketCmd` payload: the player to place into the ranked queue and
/// the matchmaking inputs used to seed the ticket. Field names are the queue's
/// `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`EnqueueTicket::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`MatchmakingTicket::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnqueueTicket {
    /// Identity of the ticket being enqueued; must name the ticket this
    /// aggregate records.
    pub ticket_id: String,
    /// The player being placed into the ranked queue. Must be non-empty; the
    /// ticket is owned by, and may never be paired against, this player.
    pub player_id: String,
    /// The player's Rating, around which the primary ±[`PRIMARY_RATING_BAND`]
    /// search band is centered.
    pub rating: u32,
    /// The player's Level, around which the primary ±[`PRIMARY_LEVEL_BAND`]
    /// search band is centered.
    pub level: u32,
    /// Seconds the ticket has already spent queued at the moment of enqueue
    /// (normally `0`). Must be below the fallback cap — a ticket already past the
    /// cap should fall back to exhibition, not enter the queue afresh.
    pub queue_time: u64,
}

impl EnqueueTicket {
    /// The command name this maps to.
    pub const COMMAND: &'static str = ENQUEUE_TICKET;

    /// Build a command enqueueing `player_id` under ticket `ticket_id` at the
    /// given `rating`, `level`, and `queue_time`.
    pub fn new(
        ticket_id: impl Into<String>,
        player_id: impl Into<String>,
        rating: u32,
        level: u32,
        queue_time: u64,
    ) -> Self {
        Self {
            ticket_id: ticket_id.into(),
            player_id: player_id.into(),
            rating,
            level,
            queue_time,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`MatchmakingTicket::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("EnqueueTicket is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `FallbackToExhibitionCmd` payload: the ticket to route to exhibition and
/// the single opponent to pair it against. Field names are the queue's
/// `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`FallbackToExhibition::into_command`], or decode it from a command payload
/// via [`serde_json`] inside [`MatchmakingTicket::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FallbackToExhibition {
    /// Identity of the ticket being routed to exhibition; must name the ticket
    /// this aggregate records.
    pub ticket_id: String,
    /// The single opponent player this ticket is paired against in the
    /// exhibition game. Must be non-empty and must not be the ticket's own
    /// player.
    pub exhibition_opponent: String,
}

impl FallbackToExhibition {
    /// The command name this maps to.
    pub const COMMAND: &'static str = FALLBACK_TO_EXHIBITION;

    /// Build a command routing `ticket_id` to an exhibition against
    /// `exhibition_opponent`.
    pub fn new(ticket_id: impl Into<String>, exhibition_opponent: impl Into<String>) -> Self {
        Self {
            ticket_id: ticket_id.into(),
            exhibition_opponent: exhibition_opponent.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`MatchmakingTicket::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload =
            serde_json::to_vec(self).expect("FallbackToExhibition is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The enqueued ticket, carried by [`Event::TicketEnqueued`] and thus by the
/// emitted `ticket.enqueued` event. Records the player placed into the queue, the
/// matchmaking inputs, and the initial primary search bands seeded on enqueue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketEnqueued {
    /// The ticket that entered the ranked queue.
    pub ticket_id: String,
    /// The player the ticket was enqueued for.
    pub player_id: String,
    /// The player's Rating at enqueue time.
    pub rating: u32,
    /// The player's Level at enqueue time.
    pub level: u32,
    /// Seconds already queued at enqueue (normally `0`).
    pub queue_time: u64,
    /// Initial Rating search band the ticket entered the queue with.
    pub rating_band: u32,
    /// Initial Level search band the ticket entered the queue with.
    pub level_band: u32,
}

/// The exhibition pairing, carried by [`Event::FellBackToExhibition`] and thus by
/// the emitted `ticket.fell.back.to.exhibition` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FellBackToExhibition {
    /// The ticket that was routed to exhibition.
    pub ticket_id: String,
    /// The single opponent the ticket was paired against.
    pub exhibition_opponent: String,
}

/// Domain events emitted by [`MatchmakingTicket`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A player was placed into the ranked queue with the primary search bands.
    TicketEnqueued(TicketEnqueued),
    /// An unmatched ticket exceeded the cap and was routed to an exhibition game.
    FellBackToExhibition(FellBackToExhibition),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::TicketEnqueued(_) => "ticket.enqueued",
            Event::FellBackToExhibition(_) => "ticket.fell.back.to.exhibition",
        }
    }
}

/// The MatchmakingTicket aggregate: one player's standing request to be paired
/// into a game.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the server — is unchanged, while it now carries the
/// ticket's matchmaking state: the requesting player, its lifecycle status, how
/// long it has queued unmatched, and its current search bands. Its `execute`
/// handles [`FallbackToExhibitionCmd`].
///
/// A fresh ticket from [`MatchmakingTicket::new`] is intentionally *not
/// fallback-ready* (it has only just entered the queue, so the cap has not
/// elapsed); the configuration methods below drive it to the state a command
/// validates, exactly as [`ExpansionSet`](crate::expansion_set) is built up
/// before a command validates it.
#[derive(Debug)]
pub struct MatchmakingTicket {
    id: String,
    root: AggregateRoot,
    /// The player who owns this ticket. A ticket may never be paired against
    /// this player.
    player_id: String,
    /// Lifecycle status; only a [`TicketStatus::Queued`] ticket is re-matchable.
    status: TicketStatus,
    /// Seconds the ticket has spent unmatched in the queue. Fallback is only
    /// permitted once this reaches [`FALLBACK_CAP_SECONDS`].
    queued_seconds: u64,
    /// Current Rating search band; must be at least [`PRIMARY_RATING_BAND`]
    /// (bands only widen as the ticket ages).
    rating_band: u32,
    /// Current Level search band; must be at least [`PRIMARY_LEVEL_BAND`].
    level_band: u32,
    /// The opponent the ticket is paired against, if any. A ticket entering the
    /// queue must be unpaired ([`None`]); it may later be paired with exactly one
    /// opponent, and never with its own player.
    opponent: Option<String>,
}

impl MatchmakingTicket {
    /// Create a new, freshly-queued ticket identified by `id`. It starts at the
    /// primary search bands and is not yet fallback-ready (the cap has not
    /// elapsed). Use the configuration methods to drive it to the state a
    /// command validates.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            player_id: String::new(),
            status: TicketStatus::Queued,
            queued_seconds: 0,
            rating_band: PRIMARY_RATING_BAND,
            level_band: PRIMARY_LEVEL_BAND,
            opponent: None,
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The player who owns this ticket.
    pub fn player_id(&self) -> &str {
        &self.player_id
    }

    /// Current status of the ticket.
    pub fn status(&self) -> TicketStatus {
        self.status
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// Set the player who owns this ticket.
    pub fn set_player(&mut self, player_id: impl Into<String>) {
        self.player_id = player_id.into();
    }

    /// Move the ticket to a new lifecycle status.
    pub fn set_status(&mut self, status: TicketStatus) {
        self.status = status;
    }

    /// Record how long the ticket has queued unmatched, in seconds.
    pub fn set_queued_seconds(&mut self, seconds: u64) {
        self.queued_seconds = seconds;
    }

    /// Pair the ticket against an opponent (or clear the pairing with [`None`]). A
    /// ticket must be unpaired to enter the queue.
    pub fn set_opponent(&mut self, opponent: Option<impl Into<String>>) {
        self.opponent = opponent.map(Into::into);
    }

    /// Set the current Rating and Level search bands. As the ticket ages these
    /// may only widen; the monotonic-band invariant rejects any band narrower
    /// than the primary width.
    pub fn set_search_bands(&mut self, rating_band: u32, level_band: u32) {
        self.rating_band = rating_band;
        self.level_band = level_band;
    }

    /// Search-band invariant: primary targeting is ±150 Rating and ±5 Level, and
    /// bands expand monotonically as the ticket ages — they may widen but never
    /// shrink below the primary width.
    fn ensure_bands_expand_monotonically(&self) -> Result<(), DomainError> {
        if self.rating_band < PRIMARY_RATING_BAND {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' Rating band {} is narrower than the primary ±{} band; search bands \
                 expand monotonically as the ticket ages and may never shrink",
                self.id, self.rating_band, PRIMARY_RATING_BAND
            )));
        }
        if self.level_band < PRIMARY_LEVEL_BAND {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' Level band {} is narrower than the primary ±{} band; search bands \
                 expand monotonically as the ticket ages and may never shrink",
                self.id, self.level_band, PRIMARY_LEVEL_BAND
            )));
        }
        Ok(())
    }

    /// Fallback-cap invariant: a ticket must fall back to exhibition only after
    /// 5 minutes ([`FALLBACK_CAP_SECONDS`]) of unmatched queueing.
    fn ensure_fallback_cap_elapsed(&self) -> Result<(), DomainError> {
        if self.queued_seconds < FALLBACK_CAP_SECONDS {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' has queued {}s but must go unmatched for {}s (5 minutes) before \
                 falling back to exhibition",
                self.id, self.queued_seconds, FALLBACK_CAP_SECONDS
            )));
        }
        Ok(())
    }

    /// Terminal-state invariant: a cancelled or matched ticket cannot be
    /// re-matched; only a queued ticket may fall back to exhibition.
    fn ensure_rematchable(&self) -> Result<(), DomainError> {
        if self.status != TicketStatus::Queued {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' is {:?} and cannot be re-matched; only a queued ticket may fall back \
                 to exhibition",
                self.id, self.status
            )));
        }
        Ok(())
    }

    /// Pairing invariant: a ticket may be paired with exactly one opponent and
    /// never with the ticket's own player.
    fn ensure_valid_exhibition_pairing(&self, opponent: &str) -> Result<(), DomainError> {
        if opponent.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' must be paired with exactly one exhibition opponent",
                self.id
            )));
        }
        if opponent == self.player_id {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' may never be paired with its own player '{}'",
                self.id, self.player_id
            )));
        }
        Ok(())
    }

    /// Fallback-cap invariant, enqueue side: a ticket that has already gone
    /// unmatched for the full 5 minutes ([`FALLBACK_CAP_SECONDS`]) belongs in an
    /// exhibition fallback, not freshly in the ranked queue. Enqueue is therefore
    /// only permitted while the ticket is still *within* the cap.
    fn ensure_within_fallback_cap(queue_time: u64) -> Result<(), DomainError> {
        if queue_time >= FALLBACK_CAP_SECONDS {
            return Err(DomainError::InvariantViolation(format!(
                "cannot enqueue a ticket already queued {queue_time}s; a ticket must fall back to \
                 exhibition after {FALLBACK_CAP_SECONDS}s (5 minutes) of unmatched queueing"
            )));
        }
        Ok(())
    }

    /// Pairing invariant, enqueue side: a ticket may be paired with exactly one
    /// opponent and never with its own player, so a ticket entering the queue to
    /// *find* an opponent must not already carry one.
    fn ensure_unpaired_for_enqueue(&self) -> Result<(), DomainError> {
        if let Some(opponent) = &self.opponent {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' is already paired with '{}' and cannot be enqueued; a ticket enters \
                 the queue unpaired and is later paired with exactly one opponent",
                self.id, opponent
            )));
        }
        Ok(())
    }

    /// Handle `EnqueueTicketCmd`: verify the command targets this ticket and names
    /// a player, enforce every invariant (re-matchable, within the fallback cap,
    /// monotonic bands, and not already paired), then seed the ticket with the
    /// player and initial queue time and emit [`Event::TicketEnqueued`].
    fn enqueue_ticket(&mut self, cmd: EnqueueTicket) -> Result<Vec<Event>, DomainError> {
        // The command must name the ticket this aggregate actually records.
        if cmd.ticket_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets ticket '{}' but this aggregate records '{}'",
                cmd.ticket_id, self.id
            )));
        }
        // A ticket must be enqueued for a named player.
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' must be enqueued for a non-empty player",
                self.id
            )));
        }

        // Enforce every invariant before placing anything into the queue.
        self.ensure_rematchable()?;
        Self::ensure_within_fallback_cap(cmd.queue_time)?;
        self.ensure_bands_expand_monotonically()?;
        self.ensure_unpaired_for_enqueue()?;

        // Seed the ticket: it is now owned by this player and has queued for the
        // stated time; the search bands it entered the queue with are recorded on
        // the event.
        self.player_id = cmd.player_id.clone();
        self.queued_seconds = cmd.queue_time;
        self.status = TicketStatus::Queued;

        let event = Event::TicketEnqueued(TicketEnqueued {
            ticket_id: cmd.ticket_id,
            player_id: cmd.player_id,
            rating: cmd.rating,
            level: cmd.level,
            queue_time: cmd.queue_time,
            rating_band: self.rating_band,
            level_band: self.level_band,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `FallbackToExhibitionCmd`: verify the command targets this ticket,
    /// enforce every invariant (re-matchable, cap elapsed, monotonic bands, and a
    /// single valid opponent), and emit [`Event::FellBackToExhibition`].
    fn fallback_to_exhibition(
        &mut self,
        cmd: FallbackToExhibition,
    ) -> Result<Vec<Event>, DomainError> {
        // The command must name the ticket this aggregate actually records.
        if cmd.ticket_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets ticket '{}' but this aggregate records '{}'",
                cmd.ticket_id, self.id
            )));
        }

        // Enforce every invariant before routing anything to exhibition.
        self.ensure_rematchable()?;
        self.ensure_fallback_cap_elapsed()?;
        self.ensure_bands_expand_monotonically()?;
        self.ensure_valid_exhibition_pairing(&cmd.exhibition_opponent)?;

        let event = Event::FellBackToExhibition(FellBackToExhibition {
            ticket_id: cmd.ticket_id,
            exhibition_opponent: cmd.exhibition_opponent,
        });
        self.status = TicketStatus::FellBackToExhibition;
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for MatchmakingTicket {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            ENQUEUE_TICKET => {
                let cmd: EnqueueTicket = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!(
                        "malformed EnqueueTicketCmd payload: {e}"
                    ))
                })?;
                self.enqueue_ticket(cmd)
            }
            FALLBACK_TO_EXHIBITION => {
                let cmd: FallbackToExhibition =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed FallbackToExhibitionCmd payload: {e}"
                        ))
                    })?;
                self.fallback_to_exhibition(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`MatchmakingTicket`] aggregate. Adapters implement
/// [`shared::Repository`] for `MatchmakingTicket` and then this marker trait.
pub trait MatchmakingTicketRepository: Repository<MatchmakingTicket> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fallback-ready ticket `t-01` for player `p-self`: queued past the cap,
    /// at (or above) the primary search bands, still actively queued. Tests
    /// mutate one aspect at a time to drive a specific rejection.
    fn ready_ticket() -> MatchmakingTicket {
        let mut ticket = MatchmakingTicket::new("t-01");
        ticket.set_player("p-self");
        ticket.set_status(TicketStatus::Queued);
        ticket.set_queued_seconds(FALLBACK_CAP_SECONDS);
        ticket.set_search_bands(PRIMARY_RATING_BAND + 50, PRIMARY_LEVEL_BAND + 2);
        ticket
    }

    /// A command routing `t-01` to an exhibition against `p-rival`.
    fn valid_cmd() -> FallbackToExhibition {
        FallbackToExhibition::new("t-01", "p-rival")
    }

    /// A fresh, enqueue-ready ticket `t-01`: just created, at the primary search
    /// bands, still queued and unpaired. Tests mutate one aspect to drive a
    /// specific enqueue rejection.
    fn enqueue_ready_ticket() -> MatchmakingTicket {
        MatchmakingTicket::new("t-01")
    }

    /// A valid `EnqueueTicketCmd` placing player `p-self` into the queue.
    fn valid_enqueue_cmd() -> EnqueueTicket {
        EnqueueTicket::new("t-01", "p-self", 1500, 30, 0)
    }

    // Scenario: Successfully execute EnqueueTicketCmd.
    #[test]
    fn enqueues_and_emits_ticket_enqueued_event() {
        let mut ticket = enqueue_ready_ticket();

        let events = ticket
            .execute(valid_enqueue_cmd().into_command())
            .expect("valid enqueue should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "ticket.enqueued");
        match &events[0] {
            Event::TicketEnqueued(enqueued) => {
                assert_eq!(enqueued.ticket_id, "t-01");
                assert_eq!(enqueued.player_id, "p-self");
                assert_eq!(enqueued.rating, 1500);
                assert_eq!(enqueued.level, 30);
                assert_eq!(enqueued.queue_time, 0);
                // The ticket entered the queue at the primary search bands.
                assert_eq!(enqueued.rating_band, PRIMARY_RATING_BAND);
                assert_eq!(enqueued.level_band, PRIMARY_LEVEL_BAND);
            }
            other => panic!("expected TicketEnqueued, got {other:?}"),
        }
        // The ticket is now owned by the player and recorded the event.
        assert_eq!(ticket.player_id(), "p-self");
        assert_eq!(ticket.status(), TicketStatus::Queued);
        assert_eq!(ticket.version(), 1);
        assert_eq!(ticket.uncommitted_events().len(), 1);
        assert_eq!(
            ticket.uncommitted_events()[0].event_type(),
            "ticket.enqueued"
        );
    }

    // Scenario: rejected — primary targeting is ±150 Rating; secondary is ±5
    // Level; bands expand monotonically as the ticket ages.
    #[test]
    fn enqueue_rejects_when_initial_band_is_narrower_than_primary() {
        let mut ticket = enqueue_ready_ticket();
        // Entering the queue below the primary ±150 band would break the
        // monotonic-band invariant.
        ticket.set_search_bands(PRIMARY_RATING_BAND - 1, PRIMARY_LEVEL_BAND);

        let err = ticket
            .execute(valid_enqueue_cmd().into_command())
            .expect_err("a sub-primary search band must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // Scenario: rejected — a ticket must fall back to exhibition after 5 minutes
    // of unmatched queueing.
    #[test]
    fn enqueue_rejects_when_already_past_the_fallback_cap() {
        let mut ticket = enqueue_ready_ticket();
        // A ticket already past the cap belongs in exhibition, not the queue.
        let cmd = EnqueueTicket::new("t-01", "p-self", 1500, 30, FALLBACK_CAP_SECONDS);

        let err = ticket
            .execute(cmd.into_command())
            .expect_err("enqueueing a ticket past the fallback cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // Scenario: rejected — a ticket may be paired with exactly one opponent and
    // never with the ticket's own player.
    #[test]
    fn enqueue_rejects_when_ticket_is_already_paired() {
        let mut ticket = enqueue_ready_ticket();
        // A ticket entering the queue must be unpaired.
        ticket.set_opponent(Some("p-rival"));

        let err = ticket
            .execute(valid_enqueue_cmd().into_command())
            .expect_err("enqueueing an already-paired ticket must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // Scenario: rejected — a cancelled or matched ticket cannot be re-matched.
    #[test]
    fn enqueue_rejects_when_ticket_is_terminal() {
        let mut ticket = enqueue_ready_ticket();
        // A cancelled ticket is terminal and cannot be re-matched into the queue.
        ticket.set_status(TicketStatus::Cancelled);

        let err = ticket
            .execute(valid_enqueue_cmd().into_command())
            .expect_err("re-enqueueing a cancelled ticket must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // A ticket must be enqueued for a named player.
    #[test]
    fn enqueue_rejects_empty_player() {
        let mut ticket = enqueue_ready_ticket();
        let cmd = EnqueueTicket::new("t-01", "   ", 1500, 30, 0);

        let err = ticket
            .execute(cmd.into_command())
            .expect_err("an empty player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // A command naming a different ticket is rejected before any invariant runs.
    #[test]
    fn enqueue_rejects_command_for_a_different_ticket() {
        let mut ticket = enqueue_ready_ticket();
        let cmd = EnqueueTicket::new("t-99", "p-self", 1500, 30, 0);

        let err = ticket
            .execute(cmd.into_command())
            .expect_err("a command for another ticket must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    #[test]
    fn enqueue_command_payload_round_trips() {
        let cmd = valid_enqueue_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, EnqueueTicket::COMMAND);
        let decoded: EnqueueTicket = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_enqueue_cmd());
    }

    // Scenario: Successfully execute FallbackToExhibitionCmd.
    #[test]
    fn falls_back_and_emits_fell_back_to_exhibition_event() {
        let mut ticket = ready_ticket();

        let events = ticket
            .execute(valid_cmd().into_command())
            .expect("valid fallback should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "ticket.fell.back.to.exhibition");
        match &events[0] {
            Event::FellBackToExhibition(paired) => {
                assert_eq!(paired.ticket_id, "t-01");
                assert_eq!(paired.exhibition_opponent, "p-rival");
            }
            other => panic!("expected FellBackToExhibition, got {other:?}"),
        }
        // The ticket transitioned out of the queue and recorded the event.
        assert_eq!(ticket.status(), TicketStatus::FellBackToExhibition);
        assert_eq!(ticket.version(), 1);
        assert_eq!(ticket.uncommitted_events().len(), 1);
        assert_eq!(
            ticket.uncommitted_events()[0].event_type(),
            "ticket.fell.back.to.exhibition"
        );
    }

    // Scenario: rejected — primary targeting is ±150 Rating; secondary is ±5
    // Level; bands expand monotonically as the ticket ages.
    #[test]
    fn rejects_when_search_band_is_narrower_than_primary() {
        let mut ticket = ready_ticket();
        // A Rating band narrower than the primary ±150 would mean the bands
        // shrank rather than expanded monotonically.
        ticket.set_search_bands(PRIMARY_RATING_BAND - 1, PRIMARY_LEVEL_BAND);

        let err = ticket
            .execute(valid_cmd().into_command())
            .expect_err("a shrunken search band must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // Scenario: rejected — a ticket must fall back to exhibition after 5 minutes
    // of unmatched queueing.
    #[test]
    fn rejects_when_fallback_cap_has_not_elapsed() {
        let mut ticket = ready_ticket();
        // One second short of the 5-minute cap.
        ticket.set_queued_seconds(FALLBACK_CAP_SECONDS - 1);

        let err = ticket
            .execute(valid_cmd().into_command())
            .expect_err("falling back before the cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // Scenario: rejected — a ticket may be paired with exactly one opponent and
    // never with the ticket's own player.
    #[test]
    fn rejects_when_paired_with_its_own_player() {
        let mut ticket = ready_ticket();
        // Pairing the ticket against its own player is never allowed.
        let cmd = FallbackToExhibition::new("t-01", "p-self");

        let err = ticket
            .execute(cmd.into_command())
            .expect_err("pairing with the ticket's own player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // Pairing invariant: exactly one opponent — an empty opponent is rejected.
    #[test]
    fn rejects_when_no_opponent_is_named() {
        let mut ticket = ready_ticket();
        let cmd = FallbackToExhibition::new("t-01", "   ");

        let err = ticket
            .execute(cmd.into_command())
            .expect_err("a missing exhibition opponent must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // Scenario: rejected — a cancelled or matched ticket cannot be re-matched.
    #[test]
    fn rejects_when_ticket_is_already_matched() {
        let mut ticket = ready_ticket();
        // A matched ticket is terminal and cannot be re-matched.
        ticket.set_status(TicketStatus::Matched);

        let err = ticket
            .execute(valid_cmd().into_command())
            .expect_err("re-matching a matched ticket must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    #[test]
    fn rejects_when_ticket_is_cancelled() {
        let mut ticket = ready_ticket();
        // A cancelled ticket is likewise terminal.
        ticket.set_status(TicketStatus::Cancelled);

        let err = ticket
            .execute(valid_cmd().into_command())
            .expect_err("re-matching a cancelled ticket must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // A command naming a different ticket is rejected before any invariant runs.
    #[test]
    fn rejects_command_for_a_different_ticket() {
        let mut ticket = ready_ticket();
        let cmd = FallbackToExhibition::new("t-99", "p-rival");

        let err = ticket
            .execute(cmd.into_command())
            .expect_err("a command for another ticket must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut ticket = MatchmakingTicket::new("t-01");
        let err = ticket.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "MatchmakingTicket");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, FallbackToExhibition::COMMAND);
        let decoded: FallbackToExhibition = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
