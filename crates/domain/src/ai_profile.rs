//! AIProfile bounded context (story-and-ai).
//!
//! An [`AIProfile`] configures how a computer opponent chooses its next action
//! for the current board state. Three invariants are re-checked whenever a move
//! is selected:
//!
//! 1. A difficulty tier maps to exactly one strategy kind (scripted for the
//!    prologue; MCTS for Standard/Brutal/Legendary).
//! 2. MCTS move selection must stay within its configured search budget.
//! 3. Scripted profiles are deterministic for a given mission and state.
//!
//! [`SelectMove`] (`SelectMoveCmd`) chooses the AI's next action for the
//! supplied board state. On success the aggregate applies and records the
//! resulting `ai.move.selected` event.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`].
const AGGREGATE_TYPE: &str = "AIProfile";

/// The command name that selects the AI's next move.
const SELECT_MOVE: &str = "SelectMoveCmd";

/// The `SelectMoveCmd` payload. Field names use the service's `camelCase`
/// schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectMove {
    /// The AIProfile choosing the move; must name this aggregate and must be
    /// non-empty.
    pub profile_id: String,
    /// The current board state the move is chosen for; must be non-empty.
    pub board_state: String,
}

impl SelectMove {
    /// The command name this maps to.
    pub const COMMAND: &'static str = SELECT_MOVE;

    /// Build a command selecting a move for `profile_id` from `board_state`.
    pub fn new(profile_id: impl Into<String>, board_state: impl Into<String>) -> Self {
        Self {
            profile_id: profile_id.into(),
            board_state: board_state.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload.
    pub fn into_command(&self) -> Command {
        let payload = serde_json::to_vec(self).expect("SelectMove is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// Story-facing alias for the command payload type.
pub type SelectMoveCmd = SelectMove;

/// The selected move carried by [`Event::MoveSelected`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveSelected {
    /// The AIProfile that selected the move.
    pub profile_id: String,
    /// The board state the move was chosen for.
    pub board_state: String,
}

/// Domain events emitted by [`AIProfile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The AI chose its next action for the supplied board state.
    MoveSelected(MoveSelected),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::MoveSelected(_) => "ai.move.selected",
        }
    }
}

/// An AI profile aggregate.
#[derive(Debug)]
pub struct AIProfile {
    id: String,
    root: AggregateRoot,
    /// Whether the profile's difficulty tier maps to exactly one strategy kind
    /// (scripted for prologue; MCTS for Standard/Brutal/Legendary).
    strategy_kind_consistent: bool,
    /// Whether MCTS move selection stays within its configured search budget.
    within_search_budget: bool,
    /// Whether the scripted profile is deterministic for a given mission and
    /// state.
    scripted_deterministic: bool,
    /// Number of moves selected so far on this profile.
    moves_selected: u64,
}

impl AIProfile {
    /// Create a new AI profile identified by `id`, valid to select moves.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            strategy_kind_consistent: true,
            within_search_budget: true,
            scripted_deterministic: true,
            moves_selected: 0,
        }
    }

    /// This aggregate's identity.
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

    /// Number of moves selected on this profile.
    pub fn moves_selected(&self) -> u64 {
        self.moves_selected
    }

    /// Model whether the difficulty tier maps to exactly one strategy kind.
    pub fn set_strategy_kind_consistent(&mut self, consistent: bool) {
        self.strategy_kind_consistent = consistent;
    }

    /// Model whether MCTS move selection stays within its configured search
    /// budget.
    pub fn set_within_search_budget(&mut self, within: bool) {
        self.within_search_budget = within;
    }

    /// Model whether the scripted profile is deterministic for a given mission
    /// and state.
    pub fn set_scripted_deterministic(&mut self, deterministic: bool) {
        self.scripted_deterministic = deterministic;
    }

    /// Strategy-mapping invariant: a difficulty tier maps to exactly one
    /// strategy kind (scripted for prologue; MCTS for Standard/Brutal/Legendary).
    fn ensure_strategy_kind_consistent(&self) -> Result<(), DomainError> {
        if !self.strategy_kind_consistent {
            return Err(DomainError::InvariantViolation(format!(
                "ai profile '{}' has a difficulty tier that does not map to exactly one strategy \
                 kind; a difficulty tier maps to exactly one strategy kind (scripted for prologue; \
                 MCTS for Standard/Brutal/Legendary)",
                self.id
            )));
        }
        Ok(())
    }

    /// Search-budget invariant: MCTS move selection stays within its configured
    /// search budget.
    fn ensure_within_search_budget(&self) -> Result<(), DomainError> {
        if !self.within_search_budget {
            return Err(DomainError::InvariantViolation(format!(
                "ai profile '{}' exceeded its configured search budget; MCTS move selection must \
                 stay within its configured search budget",
                self.id
            )));
        }
        Ok(())
    }

    /// Determinism invariant: scripted profiles are deterministic for a given
    /// mission and state.
    fn ensure_scripted_deterministic(&self) -> Result<(), DomainError> {
        if !self.scripted_deterministic {
            return Err(DomainError::InvariantViolation(format!(
                "ai profile '{}' produced a non-deterministic scripted choice; scripted profiles \
                 are deterministic for a given mission and state",
                self.id
            )));
        }
        Ok(())
    }

    /// Apply an event to aggregate state.
    fn apply(&mut self, event: &Event) {
        match event {
            Event::MoveSelected(_) => {
                self.moves_selected = self.moves_selected.saturating_add(1);
            }
        }
    }

    /// Handle `SelectMoveCmd`.
    fn select_move(&mut self, cmd: SelectMove) -> Result<Vec<Event>, DomainError> {
        if cmd.profile_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "ai profile '{}' requires a valid profileId to select a move",
                self.id
            )));
        }
        if cmd.board_state.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "ai profile '{}' requires a valid boardState to select a move",
                self.id
            )));
        }
        if cmd.profile_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets ai profile '{}' but this aggregate is ai profile '{}'",
                cmd.profile_id, self.id
            )));
        }

        self.ensure_strategy_kind_consistent()?;
        self.ensure_within_search_budget()?;
        self.ensure_scripted_deterministic()?;

        let event = Event::MoveSelected(MoveSelected {
            profile_id: cmd.profile_id,
            board_state: cmd.board_state,
        });
        self.apply(&event);
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for AIProfile {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            SELECT_MOVE => {
                let cmd: SelectMove = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed SelectMoveCmd payload: {e}"))
                })?;
                self.select_move(cmd)
            }
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`AIProfile`] aggregate.
pub trait AIProfileRepository: Repository<AIProfile> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_profile() -> AIProfile {
        let mut profile = AIProfile::new("profile-01");
        profile.set_strategy_kind_consistent(true);
        profile.set_within_search_budget(true);
        profile.set_scripted_deterministic(true);
        profile
    }

    fn valid_cmd() -> SelectMove {
        SelectMove::new("profile-01", "board:e4")
    }

    // Scenario: Successfully execute SelectMoveCmd.
    #[test]
    fn selects_move_and_emits_event() {
        let mut profile = ready_profile();

        let events = profile
            .execute(valid_cmd().into_command())
            .expect("valid move selection should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "ai.move.selected");
        match &events[0] {
            Event::MoveSelected(selected) => {
                assert_eq!(selected.profile_id, "profile-01");
                assert_eq!(selected.board_state, "board:e4");
            }
        }
        assert_eq!(profile.moves_selected(), 1);
        assert_eq!(profile.version(), 1);
        assert_eq!(profile.uncommitted_events().len(), 1);
        assert_eq!(
            profile.uncommitted_events()[0].event_type(),
            "ai.move.selected"
        );
    }

    // Scenario: SelectMoveCmd rejected - A difficulty tier maps to exactly one
    // strategy kind (scripted for prologue; MCTS for Standard/Brutal/Legendary).
    #[test]
    fn rejects_when_strategy_kind_is_inconsistent() {
        let mut profile = ready_profile();
        profile.set_strategy_kind_consistent(false);

        let err = profile.execute(valid_cmd().into_command()).expect_err(
            "a difficulty tier mapping to more than one strategy kind must be rejected",
        );

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.moves_selected(), 0);
        assert_eq!(profile.version(), 0);
    }

    // Scenario: SelectMoveCmd rejected - MCTS move selection must stay within
    // its configured search budget.
    #[test]
    fn rejects_when_search_budget_is_exceeded() {
        let mut profile = ready_profile();
        profile.set_within_search_budget(false);

        let err = profile
            .execute(valid_cmd().into_command())
            .expect_err("an MCTS selection exceeding its search budget must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.moves_selected(), 0);
        assert_eq!(profile.version(), 0);
    }

    // Scenario: SelectMoveCmd rejected - Scripted profiles are deterministic for
    // a given mission and state.
    #[test]
    fn rejects_when_scripted_choice_is_non_deterministic() {
        let mut profile = ready_profile();
        profile.set_scripted_deterministic(false);

        let err = profile
            .execute(valid_cmd().into_command())
            .expect_err("a non-deterministic scripted profile must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.moves_selected(), 0);
        assert_eq!(profile.version(), 0);
    }

    #[test]
    fn rejects_command_for_a_different_profile() {
        let mut profile = ready_profile();

        let err = profile
            .execute(SelectMove::new("profile-99", "board:e4").into_command())
            .expect_err("a command for another profile must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.moves_selected(), 0);
        assert_eq!(profile.version(), 0);
    }

    #[test]
    fn rejects_missing_profile_id() {
        let mut profile = ready_profile();

        let err = profile
            .execute(SelectMove::new("   ", "board:e4").into_command())
            .expect_err("missing profileId must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    #[test]
    fn rejects_missing_board_state() {
        let mut profile = ready_profile();

        let err = profile
            .execute(SelectMove::new("profile-01", "   ").into_command())
            .expect_err("missing boardState must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    #[test]
    fn selects_successive_moves() {
        let mut profile = ready_profile();

        profile
            .execute(valid_cmd().into_command())
            .expect("first move selection should succeed");
        profile
            .execute(SelectMove::new("profile-01", "board:e5").into_command())
            .expect("second move selection should succeed");

        assert_eq!(profile.moves_selected(), 2);
        assert_eq!(profile.version(), 2);
    }

    #[test]
    fn rejects_unknown_command() {
        let mut profile = AIProfile::new("profile-01");
        let err = profile.execute(Command::new("NoSuchCommand")).unwrap_err();

        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "AIProfile");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();

        assert_eq!(command.name, SelectMove::COMMAND);
        let decoded: SelectMove = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
