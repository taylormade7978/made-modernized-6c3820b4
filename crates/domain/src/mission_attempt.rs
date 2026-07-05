//! MissionAttempt bounded context.
//!
//! A [`MissionAttempt`] represents a player's attempt to clear a mission and
//! claim the fixed first-clear reward bundle. Four invariants are re-checked
//! when the reward claim is requested:
//!
//! 1. The fixed $MADE reward for a mission is granted only on the player's first
//!    clear, ever.
//! 2. Prologue missions are gated in sequence; a mission unlocks only after its
//!    predecessor is cleared.
//! 3. Only missions in today's Reprise rotation are eligible for repeat rewards.
//! 4. Per-mission special rules and boss HP-threshold barks fire exactly at
//!    their scripted points.
//!
//! [`ClaimFirstClearReward`] (`ClaimFirstClearRewardCmd`) claims the reward for
//! a player and mission. [`AdvanceMissionState`] (`AdvanceMissionStateCmd`)
//! advances the scripted mission state and signals barks/panels at the supplied
//! trigger. On success the aggregate applies and records the resulting event.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`].
const AGGREGATE_TYPE: &str = "MissionAttempt";

/// The command name that claims a mission's first-clear reward bundle.
const CLAIM_FIRST_CLEAR_REWARD: &str = "ClaimFirstClearRewardCmd";

/// The command name that advances scripted mission state.
const ADVANCE_MISSION_STATE: &str = "AdvanceMissionStateCmd";

/// The `ClaimFirstClearRewardCmd` payload. Field names use the service's
/// `camelCase` schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimFirstClearReward {
    /// The player claiming the reward; must be non-empty.
    pub player_id: String,
    /// The mission whose first-clear reward is claimed; must be non-empty.
    pub mission_id: String,
}

impl ClaimFirstClearReward {
    /// The command name this maps to.
    pub const COMMAND: &'static str = CLAIM_FIRST_CLEAR_REWARD;

    /// Build a command payload for `player_id` claiming `mission_id`.
    pub fn new(player_id: impl Into<String>, mission_id: impl Into<String>) -> Self {
        Self {
            player_id: player_id.into(),
            mission_id: mission_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload.
    pub fn into_command(&self) -> Command {
        let payload =
            serde_json::to_vec(self).expect("ClaimFirstClearReward is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// Story-facing alias for the command payload type.
pub type ClaimFirstClearRewardCmd = ClaimFirstClearReward;

/// The `AdvanceMissionStateCmd` payload. Field names use the service's
/// `camelCase` schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdvanceMissionState {
    /// The MissionAttempt whose scripted state is advanced; must name this
    /// aggregate and must be non-empty.
    pub mission_attempt_id: String,
    /// The scripted trigger being reached; must be non-empty.
    pub trigger: String,
}

impl AdvanceMissionState {
    /// The command name this maps to.
    pub const COMMAND: &'static str = ADVANCE_MISSION_STATE;

    /// Build a command advancing `mission_attempt_id` because `trigger` fired.
    pub fn new(mission_attempt_id: impl Into<String>, trigger: impl Into<String>) -> Self {
        Self {
            mission_attempt_id: mission_attempt_id.into(),
            trigger: trigger.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload.
    pub fn into_command(&self) -> Command {
        let payload = serde_json::to_vec(self).expect("AdvanceMissionState is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// Story-facing alias for the command payload type.
pub type AdvanceMissionStateCmd = AdvanceMissionState;

/// The first-clear reward claim carried by [`Event::FirstClearRewardClaimed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstClearRewardClaimed {
    /// The player that claimed the reward.
    pub player_id: String,
    /// The mission whose first-clear reward was claimed.
    pub mission_id: String,
}

/// The scripted-state advance carried by [`Event::MissionStateAdvanced`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionStateAdvanced {
    /// The MissionAttempt whose scripted state advanced.
    pub mission_attempt_id: String,
    /// The trigger that advanced the scripted state and fires barks/panels.
    pub trigger: String,
    /// The resulting scripted state step after the trigger is applied.
    pub scripted_state_step: u64,
}

/// Domain events emitted by [`MissionAttempt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The fixed first-clear reward bundle was claimed for a player and mission.
    FirstClearRewardClaimed(FirstClearRewardClaimed),
    /// Scripted mission state advanced and its trigger-side effects should fire.
    MissionStateAdvanced(MissionStateAdvanced),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::FirstClearRewardClaimed(_) => "first.clear.reward.claimed",
            Event::MissionStateAdvanced(_) => "mission.state.advanced",
        }
    }
}

/// A mission attempt aggregate.
#[derive(Debug)]
pub struct MissionAttempt {
    id: String,
    root: AggregateRoot,
    /// Whether this player's first-clear reward has already been claimed.
    first_clear_reward_claimed: bool,
    /// Whether a prologue mission's predecessor has been cleared.
    prologue_predecessor_cleared: bool,
    /// Whether the mission is eligible under today's Reprise rotation rules.
    reprise_rotation_eligible: bool,
    /// Whether special rules and boss HP-threshold barks fired at their scripted
    /// points for this attempt.
    scripted_points_satisfied: bool,
    /// Current scripted mission state step. Each accepted advance moves this
    /// forward by one.
    scripted_state_step: u64,
}

impl MissionAttempt {
    /// Create a new, claimable mission attempt identified by `id`.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            first_clear_reward_claimed: false,
            prologue_predecessor_cleared: true,
            reprise_rotation_eligible: true,
            scripted_points_satisfied: true,
            scripted_state_step: 0,
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

    /// Whether the first-clear reward has been claimed on this attempt.
    pub fn first_clear_reward_claimed(&self) -> bool {
        self.first_clear_reward_claimed
    }

    /// Current scripted mission state step.
    pub fn scripted_state_step(&self) -> u64 {
        self.scripted_state_step
    }

    /// Model whether the fixed first-clear reward has already been claimed.
    pub fn set_first_clear_reward_claimed(&mut self, claimed: bool) {
        self.first_clear_reward_claimed = claimed;
    }

    /// Model whether a prologue mission's predecessor has been cleared.
    pub fn set_prologue_predecessor_cleared(&mut self, cleared: bool) {
        self.prologue_predecessor_cleared = cleared;
    }

    /// Model whether the mission is in today's Reprise rotation.
    pub fn set_reprise_rotation_eligible(&mut self, eligible: bool) {
        self.reprise_rotation_eligible = eligible;
    }

    /// Model whether special rules and boss HP-threshold barks fired exactly at
    /// their scripted points.
    pub fn set_scripted_points_satisfied(&mut self, satisfied: bool) {
        self.scripted_points_satisfied = satisfied;
    }

    /// First-clear invariant: the fixed $MADE reward is granted only once.
    fn ensure_first_clear_reward_available(&self) -> Result<(), DomainError> {
        if self.first_clear_reward_claimed {
            return Err(DomainError::InvariantViolation(format!(
                "mission attempt '{}' has already claimed the fixed $MADE first-clear reward; the \
                 fixed $MADE reward for a mission is granted only on the player's first clear, ever",
                self.id
            )));
        }
        Ok(())
    }

    /// Prologue invariant: missions unlock only after their predecessor is
    /// cleared.
    fn ensure_prologue_predecessor_cleared(&self) -> Result<(), DomainError> {
        if !self.prologue_predecessor_cleared {
            return Err(DomainError::InvariantViolation(format!(
                "mission attempt '{}' is locked behind an uncleared predecessor; prologue missions \
                 are gated in sequence",
                self.id
            )));
        }
        Ok(())
    }

    /// Reprise invariant: reward eligibility follows today's Reprise rotation.
    fn ensure_reprise_rotation_eligible(&self) -> Result<(), DomainError> {
        if !self.reprise_rotation_eligible {
            return Err(DomainError::InvariantViolation(format!(
                "mission attempt '{}' is not in today's Reprise rotation; only missions in today's \
                 Reprise rotation are eligible for repeat rewards",
                self.id
            )));
        }
        Ok(())
    }

    /// Scripted-rules invariant: special rules and boss HP-threshold barks fired
    /// exactly at their scripted points.
    fn ensure_scripted_points_satisfied(&self) -> Result<(), DomainError> {
        if !self.scripted_points_satisfied {
            return Err(DomainError::InvariantViolation(format!(
                "mission attempt '{}' did not satisfy scripted mission points; per-mission special \
                 rules and boss HP-threshold barks fire exactly at their scripted points",
                self.id
            )));
        }
        Ok(())
    }

    /// Apply an event to aggregate state.
    fn apply(&mut self, event: &Event) {
        match event {
            Event::FirstClearRewardClaimed(_) => {
                self.first_clear_reward_claimed = true;
            }
            Event::MissionStateAdvanced(advanced) => {
                self.scripted_state_step = advanced.scripted_state_step;
            }
        }
    }

    /// Handle `ClaimFirstClearRewardCmd`.
    fn claim_first_clear_reward(
        &mut self,
        cmd: ClaimFirstClearReward,
    ) -> Result<Vec<Event>, DomainError> {
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "mission attempt '{}' requires a valid playerId to claim the first-clear reward",
                self.id
            )));
        }
        if cmd.mission_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "mission attempt '{}' requires a valid missionId to claim the first-clear reward",
                self.id
            )));
        }

        self.ensure_first_clear_reward_available()?;
        self.ensure_prologue_predecessor_cleared()?;
        self.ensure_reprise_rotation_eligible()?;
        self.ensure_scripted_points_satisfied()?;

        let event = Event::FirstClearRewardClaimed(FirstClearRewardClaimed {
            player_id: cmd.player_id,
            mission_id: cmd.mission_id,
        });
        self.apply(&event);
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `AdvanceMissionStateCmd`.
    fn advance_mission_state(
        &mut self,
        cmd: AdvanceMissionState,
    ) -> Result<Vec<Event>, DomainError> {
        if cmd.mission_attempt_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "mission attempt '{}' requires a valid missionAttemptId to advance mission state",
                self.id
            )));
        }
        if cmd.trigger.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "mission attempt '{}' requires a valid trigger to advance mission state",
                self.id
            )));
        }
        if cmd.mission_attempt_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets mission attempt '{}' but this aggregate is mission attempt '{}'",
                cmd.mission_attempt_id, self.id
            )));
        }

        self.ensure_first_clear_reward_available()?;
        self.ensure_prologue_predecessor_cleared()?;
        self.ensure_reprise_rotation_eligible()?;
        self.ensure_scripted_points_satisfied()?;

        let scripted_state_step = self.scripted_state_step.checked_add(1).ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "mission attempt '{}' cannot advance scripted state beyond u64::MAX",
                self.id
            ))
        })?;
        let event = Event::MissionStateAdvanced(MissionStateAdvanced {
            mission_attempt_id: cmd.mission_attempt_id,
            trigger: cmd.trigger,
            scripted_state_step,
        });
        self.apply(&event);
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for MissionAttempt {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            CLAIM_FIRST_CLEAR_REWARD => {
                let cmd: ClaimFirstClearReward =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ClaimFirstClearRewardCmd payload: {e}"
                        ))
                    })?;
                self.claim_first_clear_reward(cmd)
            }
            ADVANCE_MISSION_STATE => {
                let cmd: AdvanceMissionState =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed AdvanceMissionStateCmd payload: {e}"
                        ))
                    })?;
                self.advance_mission_state(cmd)
            }
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`MissionAttempt`] aggregate.
pub trait MissionAttemptRepository: Repository<MissionAttempt> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_attempt() -> MissionAttempt {
        let mut attempt = MissionAttempt::new("attempt-01");
        attempt.set_first_clear_reward_claimed(false);
        attempt.set_prologue_predecessor_cleared(true);
        attempt.set_reprise_rotation_eligible(true);
        attempt.set_scripted_points_satisfied(true);
        attempt
    }

    fn valid_cmd() -> ClaimFirstClearReward {
        ClaimFirstClearReward::new("player-01", "mission-01")
    }

    fn valid_advance_cmd() -> AdvanceMissionState {
        AdvanceMissionState::new("attempt-01", "boss_hp_50")
    }

    // Scenario: Successfully execute ClaimFirstClearRewardCmd.
    #[test]
    fn claims_first_clear_reward_and_emits_event() {
        let mut attempt = ready_attempt();

        let events = attempt
            .execute(valid_cmd().into_command())
            .expect("valid first-clear reward claim should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "first.clear.reward.claimed");
        match &events[0] {
            Event::FirstClearRewardClaimed(claimed) => {
                assert_eq!(claimed.player_id, "player-01");
                assert_eq!(claimed.mission_id, "mission-01");
            }
            other => panic!("expected FirstClearRewardClaimed, got {other:?}"),
        }
        assert!(attempt.first_clear_reward_claimed());
        assert_eq!(attempt.version(), 1);
        assert_eq!(attempt.uncommitted_events().len(), 1);
        assert_eq!(
            attempt.uncommitted_events()[0].event_type(),
            "first.clear.reward.claimed"
        );
    }

    // Scenario: Successfully execute AdvanceMissionStateCmd.
    #[test]
    fn advances_mission_state_and_emits_event() {
        let mut attempt = ready_attempt();

        let events = attempt
            .execute(valid_advance_cmd().into_command())
            .expect("valid mission state advance should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "mission.state.advanced");
        match &events[0] {
            Event::MissionStateAdvanced(advanced) => {
                assert_eq!(advanced.mission_attempt_id, "attempt-01");
                assert_eq!(advanced.trigger, "boss_hp_50");
                assert_eq!(advanced.scripted_state_step, 1);
            }
            other => panic!("expected MissionStateAdvanced, got {other:?}"),
        }
        assert_eq!(attempt.scripted_state_step(), 1);
        assert_eq!(attempt.version(), 1);
        assert_eq!(attempt.uncommitted_events().len(), 1);
        assert_eq!(
            attempt.uncommitted_events()[0].event_type(),
            "mission.state.advanced"
        );
    }

    // Scenario: AdvanceMissionStateCmd rejected - The fixed $MADE reward for a
    // mission is granted only on the player's first clear, ever.
    #[test]
    fn advance_rejects_when_first_clear_reward_was_already_claimed() {
        let mut attempt = ready_attempt();
        attempt.set_first_clear_reward_claimed(true);

        let err = attempt
            .execute(valid_advance_cmd().into_command())
            .expect_err("an advance violating first-clear reward rules must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.scripted_state_step(), 0);
        assert_eq!(attempt.version(), 0);
    }

    // Scenario: AdvanceMissionStateCmd rejected - Prologue missions are gated in
    // sequence; a mission unlocks only after its predecessor is cleared.
    #[test]
    fn advance_rejects_when_prologue_predecessor_is_uncleared() {
        let mut attempt = ready_attempt();
        attempt.set_prologue_predecessor_cleared(false);

        let err = attempt
            .execute(valid_advance_cmd().into_command())
            .expect_err("an advance with an uncleared predecessor must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.scripted_state_step(), 0);
        assert_eq!(attempt.version(), 0);
    }

    // Scenario: AdvanceMissionStateCmd rejected - Only missions in today's
    // Reprise rotation are eligible for repeat rewards.
    #[test]
    fn advance_rejects_when_mission_is_not_in_reprise_rotation() {
        let mut attempt = ready_attempt();
        attempt.set_reprise_rotation_eligible(false);

        let err = attempt
            .execute(valid_advance_cmd().into_command())
            .expect_err("an advance outside today's Reprise rotation must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.scripted_state_step(), 0);
        assert_eq!(attempt.version(), 0);
    }

    // Scenario: AdvanceMissionStateCmd rejected - Per-mission special rules and
    // boss HP-threshold barks fire exactly at their scripted points.
    #[test]
    fn advance_rejects_when_scripted_points_are_not_satisfied() {
        let mut attempt = ready_attempt();
        attempt.set_scripted_points_satisfied(false);

        let err = attempt
            .execute(valid_advance_cmd().into_command())
            .expect_err("an advance at the wrong scripted point must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.scripted_state_step(), 0);
        assert_eq!(attempt.version(), 0);
    }

    #[test]
    fn advance_rejects_command_for_a_different_attempt() {
        let mut attempt = ready_attempt();

        let err = attempt
            .execute(AdvanceMissionState::new("attempt-99", "boss_hp_50").into_command())
            .expect_err("an advance command for another attempt must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.scripted_state_step(), 0);
        assert_eq!(attempt.version(), 0);
    }

    #[test]
    fn advance_rejects_missing_mission_attempt_id() {
        let mut attempt = ready_attempt();

        let err = attempt
            .execute(AdvanceMissionState::new("   ", "boss_hp_50").into_command())
            .expect_err("missing missionAttemptId must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.scripted_state_step(), 0);
        assert_eq!(attempt.version(), 0);
    }

    #[test]
    fn advance_rejects_missing_trigger() {
        let mut attempt = ready_attempt();

        let err = attempt
            .execute(AdvanceMissionState::new("attempt-01", "   ").into_command())
            .expect_err("missing trigger must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.scripted_state_step(), 0);
        assert_eq!(attempt.version(), 0);
    }

    #[test]
    fn advance_command_payload_round_trips() {
        let cmd = valid_advance_cmd();
        let command = cmd.into_command();

        assert_eq!(command.name, AdvanceMissionState::COMMAND);
        let decoded: AdvanceMissionState = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_advance_cmd());
    }

    // Scenario: rejected - The fixed $MADE reward for a mission is granted only
    // on the player's first clear, ever.
    #[test]
    fn rejects_when_first_clear_reward_was_already_claimed() {
        let mut attempt = ready_attempt();
        attempt.set_first_clear_reward_claimed(true);

        let err = attempt
            .execute(valid_cmd().into_command())
            .expect_err("a repeated first-clear reward claim must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.version(), 0);
    }

    // Scenario: rejected - Prologue missions are gated in sequence; a mission
    // unlocks only after its predecessor is cleared.
    #[test]
    fn rejects_when_prologue_predecessor_is_uncleared() {
        let mut attempt = ready_attempt();
        attempt.set_prologue_predecessor_cleared(false);

        let err = attempt
            .execute(valid_cmd().into_command())
            .expect_err("an uncleared predecessor must block prologue reward claims");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.version(), 0);
    }

    // Scenario: rejected - Only missions in today's Reprise rotation are
    // eligible for repeat rewards.
    #[test]
    fn rejects_when_mission_is_not_in_reprise_rotation() {
        let mut attempt = ready_attempt();
        attempt.set_reprise_rotation_eligible(false);

        let err = attempt
            .execute(valid_cmd().into_command())
            .expect_err("a mission outside today's Reprise rotation must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.version(), 0);
    }

    // Scenario: rejected - Per-mission special rules and boss HP-threshold barks
    // fire exactly at their scripted points.
    #[test]
    fn rejects_when_scripted_points_are_not_satisfied() {
        let mut attempt = ready_attempt();
        attempt.set_scripted_points_satisfied(false);

        let err = attempt
            .execute(valid_cmd().into_command())
            .expect_err("unsatisfied scripted points must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.version(), 0);
    }

    #[test]
    fn rejects_missing_player_id() {
        let mut attempt = ready_attempt();

        let err = attempt
            .execute(ClaimFirstClearReward::new("   ", "mission-01").into_command())
            .expect_err("missing playerId must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.version(), 0);
    }

    #[test]
    fn rejects_missing_mission_id() {
        let mut attempt = ready_attempt();

        let err = attempt
            .execute(ClaimFirstClearReward::new("player-01", "   ").into_command())
            .expect_err("missing missionId must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.version(), 0);
    }

    #[test]
    fn rejects_repeat_claim_after_successful_claim() {
        let mut attempt = ready_attempt();

        attempt
            .execute(valid_cmd().into_command())
            .expect("first claim should succeed");
        let err = attempt
            .execute(valid_cmd().into_command())
            .expect_err("second claim should be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(attempt.version(), 1);
    }

    #[test]
    fn rejects_unknown_command() {
        let mut attempt = MissionAttempt::new("attempt-01");
        let err = attempt.execute(Command::new("NoSuchCommand")).unwrap_err();

        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "MissionAttempt");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();

        assert_eq!(command.name, ClaimFirstClearReward::COMMAND);
        let decoded: ClaimFirstClearReward = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
