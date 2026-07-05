//! MatchmakingTicket bounded context — a player's request to be matched into a game.
//!
//! A [`MatchmakingTicket`] is one player's standing request to be paired with an
//! opponent. While the ticket sits in the queue its *search bands* widen as it
//! ages so a match becomes progressively easier to find. Four invariants govern
//! whether a band may be widened:
//!
//! 1. **Monotonic expansion** — primary targeting is ±[`BASE_RATING_BAND`] Rating
//!    and secondary is ±[`BASE_LEVEL_BAND`] Level, and the bands only ever grow as
//!    the ticket ages: an expansion must move *forward* in elapsed time (and never
//!    shrink a band).
//! 2. **Fallback deadline** — a ticket must fall back to exhibition after
//!    [`FALLBACK_DEADLINE_SECONDS`] (5 minutes) of unmatched queueing, so it can no
//!    longer widen its bands past that deadline.
//! 3. **Legal pairing** — a ticket may be paired with exactly one opponent and
//!    never with the ticket's own player.
//! 4. **Settled tickets are final** — a cancelled or matched ticket cannot be
//!    re-matched, and therefore cannot expand its search bands.
//!
//! The only command implemented so far is [`ExpandSearchBand`]
//! (`ExpandSearchBandCmd`): it widens the rating/level bands for an aging ticket,
//! enforcing all four invariants, and on success emits
//! [`Event::SearchBandExpanded`] (`search.band.expanded`). This module is
//! hand-written (it no longer uses `shared::stub_aggregate!`) but preserves the
//! same public surface — a [`MatchmakingTicket`] aggregate and a
//! [`MatchmakingTicketRepository`] port — so the persistence adapters in
//! `crates/mocks` keep compiling unchanged.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "MatchmakingTicket";

/// The command name [`MatchmakingTicket::execute`] recognizes.
const EXPAND_SEARCH_BAND: &str = "ExpandSearchBandCmd";

/// Primary targeting band: a fresh ticket searches within ±150 Rating. Every
/// expansion widens outward from this floor and may never narrow below it.
pub const BASE_RATING_BAND: u32 = 150;

/// Secondary targeting band: a fresh ticket searches within ±5 Level. Every
/// expansion widens outward from this floor and may never narrow below it.
pub const BASE_LEVEL_BAND: u32 = 5;

/// A ticket must fall back to exhibition after 5 minutes (300 seconds) of
/// unmatched queueing; past this deadline its bands can no longer be widened.
pub const FALLBACK_DEADLINE_SECONDS: u64 = 300;

/// How often (in seconds of queue time) the bands step outward. Each elapsed
/// interval widens the rating band by [`RATING_STEP`] and the level band by
/// [`LEVEL_STEP`], giving the monotonic growth the first invariant requires.
pub const EXPANSION_INTERVAL_SECONDS: u64 = 30;

/// Rating widening applied per [`EXPANSION_INTERVAL_SECONDS`] of queue time.
pub const RATING_STEP: u32 = 50;

/// Level widening applied per [`EXPANSION_INTERVAL_SECONDS`] of queue time.
pub const LEVEL_STEP: u32 = 1;

/// The widened rating/level bands for a ticket that has been queued for
/// `elapsed_seconds`. Growth is monotonic in `elapsed_seconds`: more queue time
/// never yields a narrower band, which is what the monotonic-expansion invariant
/// relies on.
fn widened_bands(elapsed_seconds: u64) -> (u32, u32) {
    let steps = (elapsed_seconds / EXPANSION_INTERVAL_SECONDS) as u32;
    (
        BASE_RATING_BAND + steps * RATING_STEP,
        BASE_LEVEL_BAND + steps * LEVEL_STEP,
    )
}

/// Where a ticket sits in its lifecycle. Only a [`TicketStatus::Queued`] ticket
/// is still searching; a matched or cancelled ticket is settled and may not be
/// re-matched (and so may not expand its bands).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketStatus {
    /// Actively searching the queue — the only state that may expand its bands.
    Queued,
    /// Paired with an opponent; settled and no longer searching.
    Matched,
    /// Withdrawn from the queue; settled and no longer searching.
    Cancelled,
}

/// The `ExpandSearchBandCmd` payload: which ticket to widen and how long it has
/// been queued. Field names are the matchmaking schema's `camelCase`.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ExpandSearchBand::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`MatchmakingTicket::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpandSearchBand {
    /// Identity of the ticket to widen; must name the ticket this aggregate
    /// records.
    pub ticket_id: String,
    /// Seconds the ticket has been queued. Drives the widened bands and must move
    /// forward relative to the ticket's last expansion (monotonic growth).
    pub elapsed_seconds: u64,
}

impl ExpandSearchBand {
    /// The command name this maps to.
    pub const COMMAND: &'static str = EXPAND_SEARCH_BAND;

    /// Build a command widening `ticket_id`'s bands after `elapsed_seconds` of
    /// queueing.
    pub fn new(ticket_id: impl Into<String>, elapsed_seconds: u64) -> Self {
        Self {
            ticket_id: ticket_id.into(),
            elapsed_seconds,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`MatchmakingTicket::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ExpandSearchBand is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The widened search bands, carried by [`Event::SearchBandExpanded`] and thus by
/// the emitted `search.band.expanded` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchBandExpanded {
    /// The ticket whose bands were widened.
    pub ticket_id: String,
    /// The new (wider) rating band, ±Rating.
    pub rating_band: u32,
    /// The new (wider) level band, ±Level.
    pub level_band: u32,
    /// The queue time at which this expansion was applied.
    pub elapsed_seconds: u64,
}

/// Domain events emitted by [`MatchmakingTicket`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An aging ticket widened its rating/level search bands.
    SearchBandExpanded(SearchBandExpanded),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::SearchBandExpanded(_) => "search.band.expanded",
        }
    }
}

/// The MatchmakingTicket aggregate: one player's standing request to be matched.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the server — is unchanged, while it now carries the
/// ticket's matchmaking state: the queueing player, its lifecycle status, an
/// optional paired opponent, and the current search bands together with the
/// elapsed time at which they were last widened. Its `execute` handles
/// [`ExpandSearchBandCmd`].
///
/// A fresh ticket from [`MatchmakingTicket::new`] is [`TicketStatus::Queued`]
/// with no opponent and its bands at the ±[`BASE_RATING_BAND`]/±[`BASE_LEVEL_BAND`]
/// floor, ready to widen as it ages; the configuration methods below move it into
/// the other states a rejection scenario exercises.
#[derive(Debug)]
pub struct MatchmakingTicket {
    id: String,
    root: AggregateRoot,
    /// The player who owns this ticket. An opponent may never be this player.
    player_id: String,
    /// Where the ticket sits in its lifecycle; only [`TicketStatus::Queued`]
    /// tickets may expand their bands.
    status: TicketStatus,
    /// The single opponent this ticket has been paired with, if any. `None` while
    /// still searching.
    opponent: Option<String>,
    /// The current rating band (±Rating). Starts at [`BASE_RATING_BAND`] and only
    /// ever grows.
    rating_band: u32,
    /// The current level band (±Level). Starts at [`BASE_LEVEL_BAND`] and only
    /// ever grows.
    level_band: u32,
    /// Queue time at which the bands were last widened. An expansion must move
    /// strictly beyond this to keep growth monotonic.
    last_expanded_at_seconds: u64,
}

impl MatchmakingTicket {
    /// Create a new ticket identified by `id`, owned by a player of the same
    /// identity, queued with its bands at the base floor. Use the configuration
    /// methods to move it into the states a rejection scenario needs.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            player_id: id.clone(),
            id,
            root: AggregateRoot::new(),
            status: TicketStatus::Queued,
            opponent: None,
            rating_band: BASE_RATING_BAND,
            level_band: BASE_LEVEL_BAND,
            last_expanded_at_seconds: 0,
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

    /// The ticket's current lifecycle status.
    pub fn status(&self) -> TicketStatus {
        self.status
    }

    /// The current rating band (±Rating).
    pub fn rating_band(&self) -> u32 {
        self.rating_band
    }

    /// The current level band (±Level).
    pub fn level_band(&self) -> u32 {
        self.level_band
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// Set the player who owns this ticket (an opponent may never be this player).
    pub fn set_player_id(&mut self, player_id: impl Into<String>) {
        self.player_id = player_id.into();
    }

    /// Move the ticket to a settled state (matched or cancelled), after which it
    /// may no longer be re-matched.
    pub fn settle(&mut self, status: TicketStatus) {
        self.status = status;
    }

    /// Pair this ticket with `opponent`. Modeled as `Option`, so a ticket carries
    /// at most one opponent.
    pub fn pair_with(&mut self, opponent: impl Into<String>) {
        self.opponent = Some(opponent.into());
    }

    /// Record that the ticket last widened its bands at `elapsed_seconds`, growing
    /// them to `rating_band`/`level_band`. Used to drive later, monotonic
    /// expansions (a subsequent command must move strictly beyond this time).
    pub fn note_last_expansion(&mut self, elapsed_seconds: u64, rating_band: u32, level_band: u32) {
        self.last_expanded_at_seconds = elapsed_seconds;
        self.rating_band = rating_band;
        self.level_band = level_band;
    }

    /// Settled-ticket invariant: a cancelled or matched ticket cannot be
    /// re-matched, and so cannot expand its search bands.
    fn ensure_not_settled(&self) -> Result<(), DomainError> {
        match self.status {
            TicketStatus::Queued => Ok(()),
            TicketStatus::Matched => Err(DomainError::InvariantViolation(format!(
                "ticket '{}' is already matched and cannot be re-matched",
                self.id
            ))),
            TicketStatus::Cancelled => Err(DomainError::InvariantViolation(format!(
                "ticket '{}' is cancelled and cannot be re-matched",
                self.id
            ))),
        }
    }

    /// Pairing invariant: a ticket may be paired with exactly one opponent and
    /// never with the ticket's own player.
    fn ensure_pairing_legal(&self) -> Result<(), DomainError> {
        if let Some(opponent) = &self.opponent {
            if opponent == &self.player_id {
                return Err(DomainError::InvariantViolation(format!(
                    "ticket '{}' may never be paired with its own player '{}'",
                    self.id, self.player_id
                )));
            }
        }
        Ok(())
    }

    /// Fallback invariant: a ticket must fall back to exhibition after 5 minutes
    /// of unmatched queueing, so it can no longer widen its bands past the
    /// deadline.
    fn ensure_within_queue_window(&self, elapsed_seconds: u64) -> Result<(), DomainError> {
        if elapsed_seconds >= FALLBACK_DEADLINE_SECONDS {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' has queued {elapsed_seconds}s and must fall back to exhibition after \
                 {FALLBACK_DEADLINE_SECONDS}s; its bands can no longer be widened",
                self.id
            )));
        }
        Ok(())
    }

    /// Monotonic-expansion invariant: bands widen only as the ticket ages, so an
    /// expansion must move strictly forward in elapsed time and never narrow a
    /// band below its current width.
    fn ensure_monotonic_expansion(
        &self,
        elapsed_seconds: u64,
        new_rating_band: u32,
        new_level_band: u32,
    ) -> Result<(), DomainError> {
        if elapsed_seconds <= self.last_expanded_at_seconds {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' cannot expand at {elapsed_seconds}s; bands widen monotonically and it \
                 last expanded at {}s",
                self.id, self.last_expanded_at_seconds
            )));
        }
        if new_rating_band < self.rating_band || new_level_band < self.level_band {
            return Err(DomainError::InvariantViolation(format!(
                "ticket '{}' cannot narrow its search bands from ±{}/±{} to ±{new_rating_band}/\
                 ±{new_level_band}",
                self.id, self.rating_band, self.level_band
            )));
        }
        Ok(())
    }

    /// Handle `ExpandSearchBandCmd`: verify the command targets this ticket,
    /// enforce all four invariants, widen the bands, and emit
    /// [`Event::SearchBandExpanded`].
    fn expand_search_band(&mut self, cmd: ExpandSearchBand) -> Result<Vec<Event>, DomainError> {
        // The command must name the ticket this aggregate actually records.
        if cmd.ticket_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets ticket '{}' but this aggregate records '{}'",
                cmd.ticket_id, self.id
            )));
        }

        let (new_rating_band, new_level_band) = widened_bands(cmd.elapsed_seconds);

        // Enforce every invariant before widening anything.
        self.ensure_not_settled()?;
        self.ensure_pairing_legal()?;
        self.ensure_within_queue_window(cmd.elapsed_seconds)?;
        self.ensure_monotonic_expansion(cmd.elapsed_seconds, new_rating_band, new_level_band)?;

        // Apply the widened bands and advance the ticket's expansion clock.
        self.rating_band = new_rating_band;
        self.level_band = new_level_band;
        self.last_expanded_at_seconds = cmd.elapsed_seconds;

        let event = Event::SearchBandExpanded(SearchBandExpanded {
            ticket_id: cmd.ticket_id,
            rating_band: new_rating_band,
            level_band: new_level_band,
            elapsed_seconds: cmd.elapsed_seconds,
        });
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
            EXPAND_SEARCH_BAND => {
                let cmd: ExpandSearchBand =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ExpandSearchBandCmd payload: {e}"
                        ))
                    })?;
                self.expand_search_band(cmd)
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

    /// A valid, queued ticket `TCKT-01` searching at the base bands, ready to
    /// widen. Tests mutate one aspect at a time to drive a specific rejection.
    fn valid_ticket() -> MatchmakingTicket {
        MatchmakingTicket::new("TCKT-01")
    }

    /// A command widening `TCKT-01` after 90s of queueing (3 expansion steps).
    fn valid_cmd() -> ExpandSearchBand {
        ExpandSearchBand::new("TCKT-01", 90)
    }

    // Scenario: Successfully execute ExpandSearchBandCmd.
    #[test]
    fn expands_band_and_emits_search_band_expanded_event() {
        let mut ticket = valid_ticket();

        let events = ticket
            .execute(valid_cmd().into_command())
            .expect("valid expansion should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "search.band.expanded");
        match &events[0] {
            Event::SearchBandExpanded(expanded) => {
                assert_eq!(expanded.ticket_id, "TCKT-01");
                assert_eq!(expanded.elapsed_seconds, 90);
                // 90s / 30s = 3 steps: rating 150 + 3*50, level 5 + 3*1.
                assert_eq!(expanded.rating_band, 300);
                assert_eq!(expanded.level_band, 8);
            }
        }
        // The event was recorded and the bands widened on the aggregate.
        assert_eq!(ticket.version(), 1);
        assert_eq!(ticket.rating_band(), 300);
        assert_eq!(ticket.level_band(), 8);
        assert_eq!(ticket.uncommitted_events().len(), 1);
    }

    // Scenario: rejected — bands expand monotonically as the ticket ages (a
    // command that moves backwards in elapsed time must be refused).
    #[test]
    fn rejects_non_monotonic_expansion() {
        let mut ticket = valid_ticket();
        // The ticket already widened at 120s; a later command claims only 90s.
        ticket.note_last_expansion(120, 350, 9);

        let err = ticket
            .execute(valid_cmd().into_command())
            .expect_err("a non-monotonic expansion must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // Scenario: rejected — a ticket must fall back to exhibition after 5 minutes
    // of unmatched queueing.
    #[test]
    fn rejects_expansion_past_fallback_deadline() {
        let mut ticket = valid_ticket();
        // 5 minutes elapsed: the ticket must fall back, not widen.
        let cmd = ExpandSearchBand::new("TCKT-01", FALLBACK_DEADLINE_SECONDS);

        let err = ticket
            .execute(cmd.into_command())
            .expect_err("an expansion past the fallback deadline must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // Scenario: rejected — a ticket may be paired with exactly one opponent and
    // never with the ticket's own player.
    #[test]
    fn rejects_expansion_when_paired_with_own_player() {
        let mut ticket = valid_ticket();
        // Illegally paired with its own player.
        ticket.pair_with(ticket.player_id().to_string());

        let err = ticket
            .execute(valid_cmd().into_command())
            .expect_err("a self-paired ticket must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // Scenario: rejected — a cancelled or matched ticket cannot be re-matched.
    #[test]
    fn rejects_expansion_when_ticket_is_matched() {
        let mut ticket = valid_ticket();
        ticket.settle(TicketStatus::Matched);

        let err = ticket
            .execute(valid_cmd().into_command())
            .expect_err("a matched ticket must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    #[test]
    fn rejects_expansion_when_ticket_is_cancelled() {
        let mut ticket = valid_ticket();
        ticket.settle(TicketStatus::Cancelled);

        let err = ticket
            .execute(valid_cmd().into_command())
            .expect_err("a cancelled ticket must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(ticket.version(), 0);
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut ticket = MatchmakingTicket::new("TCKT-01");
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
        assert_eq!(command.name, ExpandSearchBand::COMMAND);
        let decoded: ExpandSearchBand = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
