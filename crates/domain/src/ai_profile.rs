//! AIProfile bounded context (story-and-ai).
//!
//! An [`AIProfile`] binds a mission's difficulty tier to the opponent strategy
//! that drives it and, for search-based strategies, the move-selection budget.
//! Three invariants are re-checked whenever a profile is (re)configured or its
//! difficulty is tuned:
//!
//! 1. A difficulty tier maps to exactly one strategy kind (scripted for the
//!    prologue; MCTS for Standard/Brutal/Legendary).
//! 2. MCTS move selection must stay within its configured search budget.
//! 3. Scripted profiles are deterministic for a given mission and state.
//!
//! Two commands drive the aggregate:
//!
//! * [`ConfigureAIProfile`] (`ConfigureAIProfileCmd`) binds a tier to a strategy
//!   and budget. On success the aggregate applies and records the resulting
//!   `ai.profile.configured` event.
//! * [`TuneDifficulty`] (`TuneDifficultyCmd`) adjusts the strategy budget/weights
//!   for balance. On success the aggregate applies and records the resulting
//!   `difficulty.tuned` event.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`].
const AGGREGATE_TYPE: &str = "AIProfile";

/// The command name that binds a difficulty tier to a strategy and budget.
const CONFIGURE_AI_PROFILE: &str = "ConfigureAIProfileCmd";

/// The command name that tunes a profile's difficulty budget/weights.
const TUNE_DIFFICULTY: &str = "TuneDifficultyCmd";

/// Upper bound on an MCTS profile's per-move search budget (simulations). A
/// configured budget must stay within this ceiling so move selection cannot run
/// unbounded; a budget of zero would leave MCTS with no simulations to spend.
const MAX_MCTS_BUDGET: u64 = 100_000;

/// The difficulty tier a profile drives. Each tier maps to exactly one
/// [`StrategyKind`] via [`DifficultyTier::canonical_strategy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum DifficultyTier {
    /// The scripted, hand-authored prologue tier.
    Prologue,
    /// The baseline search-driven tier.
    Standard,
    /// A harder search-driven tier.
    Brutal,
    /// The hardest search-driven tier.
    Legendary,
}

impl DifficultyTier {
    /// The single strategy kind this tier is allowed to bind to.
    ///
    /// This is the sole home of invariant 1's mapping: the prologue is scripted;
    /// every other tier is MCTS-driven.
    pub fn canonical_strategy(self) -> StrategyKind {
        match self {
            DifficultyTier::Prologue => StrategyKind::Scripted,
            DifficultyTier::Standard | DifficultyTier::Brutal | DifficultyTier::Legendary => {
                StrategyKind::Mcts
            }
        }
    }
}

/// The strategy that selects moves for a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum StrategyKind {
    /// A deterministic, hand-authored script (used by the prologue).
    Scripted,
    /// Monte-Carlo Tree Search, bounded by a configured search budget.
    Mcts,
}

/// The `ConfigureAIProfileCmd` payload. Field names use the service's
/// `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ConfigureAIProfile::into_command`], or decode it from a command payload
/// via [`serde_json`] inside [`AIProfile::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureAIProfile {
    /// The profile being configured; must name this aggregate and be non-empty.
    pub profile_id: String,
    /// The difficulty tier this profile drives.
    pub difficulty_tier: DifficultyTier,
    /// The strategy the tier binds to; must be the tier's canonical strategy.
    pub strategy_kind: StrategyKind,
    /// The MCTS per-move search budget (simulations). Meaningful only for MCTS
    /// profiles, where it must be within `(0, MAX_MCTS_BUDGET]`.
    pub mcts_budget: u64,
}

impl ConfigureAIProfile {
    /// The command name this maps to.
    pub const COMMAND: &'static str = CONFIGURE_AI_PROFILE;

    /// Build a command binding `profile_id`'s `difficulty_tier` to
    /// `strategy_kind` with the given `mcts_budget`.
    pub fn new(
        profile_id: impl Into<String>,
        difficulty_tier: DifficultyTier,
        strategy_kind: StrategyKind,
        mcts_budget: u64,
    ) -> Self {
        Self {
            profile_id: profile_id.into(),
            difficulty_tier,
            strategy_kind,
            mcts_budget,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload.
    pub fn into_command(&self) -> Command {
        let payload = serde_json::to_vec(self).expect("ConfigureAIProfile is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// Story-facing alias for the configure command payload type.
pub type ConfigureAIProfileCmd = ConfigureAIProfile;

/// The `TuneDifficultyCmd` payload. Field names use the service's `camelCase`
/// schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuneDifficulty {
    /// The AIProfile being tuned; must name this aggregate and must be
    /// non-empty.
    pub profile_id: String,
    /// The opaque tuning parameters (strategy budget/weights) to apply; must be
    /// non-empty.
    pub tuning_params: String,
}

impl TuneDifficulty {
    /// The command name this maps to.
    pub const COMMAND: &'static str = TUNE_DIFFICULTY;

    /// Build a command tuning `profile_id` with `tuning_params`.
    pub fn new(profile_id: impl Into<String>, tuning_params: impl Into<String>) -> Self {
        Self {
            profile_id: profile_id.into(),
            tuning_params: tuning_params.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload.
    pub fn into_command(&self) -> Command {
        let payload = serde_json::to_vec(self).expect("TuneDifficulty is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// Story-facing alias for the tune command payload type.
pub type TuneDifficultyCmd = TuneDifficulty;

/// The configuration carried by [`Event::AIProfileConfigured`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AIProfileConfigured {
    /// The profile that was configured.
    pub profile_id: String,
    /// The difficulty tier bound by this configuration.
    pub difficulty_tier: DifficultyTier,
    /// The strategy the tier was bound to.
    pub strategy_kind: StrategyKind,
    /// The MCTS search budget recorded for the profile.
    pub mcts_budget: u64,
}

/// The applied difficulty tuning carried by [`Event::DifficultyTuned`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DifficultyTuned {
    /// The AIProfile whose difficulty was tuned.
    pub profile_id: String,
    /// The tuning parameters (strategy budget/weights) that were applied.
    pub tuning_params: String,
}

/// Domain events emitted by [`AIProfile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A difficulty tier was bound to a strategy and budget.
    AIProfileConfigured(AIProfileConfigured),
    /// The profile's difficulty budget/weights were tuned for balance.
    DifficultyTuned(DifficultyTuned),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::AIProfileConfigured(_) => "ai.profile.configured",
            Event::DifficultyTuned(_) => "difficulty.tuned",
        }
    }
}

/// An AI opponent profile aggregate.
#[derive(Debug)]
pub struct AIProfile {
    id: String,
    root: AggregateRoot,
    /// The tier currently bound, once configured.
    difficulty_tier: Option<DifficultyTier>,
    /// The strategy currently bound, once configured.
    strategy_kind: Option<StrategyKind>,
    /// The MCTS search budget currently recorded.
    mcts_budget: u64,
    /// Whether scripted move selection can be guaranteed deterministic for the
    /// profile's mission and state. Configuration is rejected while this cannot
    /// be guaranteed (invariant 3).
    scripted_determinism_guaranteed: bool,
    /// Whether the profile's difficulty tier maps to exactly one strategy kind
    /// (scripted for prologue; MCTS for Standard/Brutal/Legendary). Consulted
    /// when tuning an already-bound profile (invariant 1).
    tier_strategy_mapping_valid: bool,
    /// Whether MCTS move selection stays within its configured search budget.
    /// Consulted when tuning an already-bound profile (invariant 2).
    mcts_within_search_budget: bool,
    /// Whether scripted profiles are deterministic for a given mission and
    /// state. Consulted when tuning an already-bound profile (invariant 3).
    scripted_deterministic: bool,
}

impl AIProfile {
    /// Create a new, unconfigured AI profile identified by `id`.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            difficulty_tier: None,
            strategy_kind: None,
            mcts_budget: 0,
            scripted_determinism_guaranteed: true,
            tier_strategy_mapping_valid: true,
            mcts_within_search_budget: true,
            scripted_deterministic: true,
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

    /// The tier currently bound, if the profile has been configured.
    pub fn difficulty_tier(&self) -> Option<DifficultyTier> {
        self.difficulty_tier
    }

    /// The strategy currently bound, if the profile has been configured.
    pub fn strategy_kind(&self) -> Option<StrategyKind> {
        self.strategy_kind
    }

    /// The MCTS search budget currently recorded.
    pub fn mcts_budget(&self) -> u64 {
        self.mcts_budget
    }

    /// Model whether scripted move selection is guaranteed deterministic for the
    /// profile's mission and state.
    pub fn set_scripted_determinism_guaranteed(&mut self, guaranteed: bool) {
        self.scripted_determinism_guaranteed = guaranteed;
    }

    /// Model whether the difficulty tier maps to exactly one strategy kind.
    pub fn set_tier_strategy_mapping_valid(&mut self, valid: bool) {
        self.tier_strategy_mapping_valid = valid;
    }

    /// Model whether MCTS move selection stays within its search budget.
    pub fn set_mcts_within_search_budget(&mut self, within: bool) {
        self.mcts_within_search_budget = within;
    }

    /// Model whether scripted profiles are deterministic for a mission and state.
    pub fn set_scripted_deterministic(&mut self, deterministic: bool) {
        self.scripted_deterministic = deterministic;
    }

    /// Mapping invariant: a difficulty tier binds to exactly one strategy kind
    /// (scripted for the prologue; MCTS for Standard/Brutal/Legendary).
    fn ensure_tier_strategy_mapping(
        &self,
        tier: DifficultyTier,
        strategy: StrategyKind,
    ) -> Result<(), DomainError> {
        let expected = tier.canonical_strategy();
        if strategy != expected {
            return Err(DomainError::InvariantViolation(format!(
                "AI profile '{}' bound tier {tier:?} to strategy {strategy:?}, but a difficulty \
                 tier maps to exactly one strategy kind (scripted for prologue; MCTS for \
                 Standard/Brutal/Legendary): tier {tier:?} requires {expected:?}",
                self.id
            )));
        }
        Ok(())
    }

    /// Budget invariant: MCTS move selection must stay within its configured
    /// search budget — the budget must be a positive value within the ceiling.
    fn ensure_mcts_within_budget(
        &self,
        strategy: StrategyKind,
        budget: u64,
    ) -> Result<(), DomainError> {
        if strategy == StrategyKind::Mcts && (budget == 0 || budget > MAX_MCTS_BUDGET) {
            return Err(DomainError::InvariantViolation(format!(
                "AI profile '{}' configured an MCTS search budget of {budget}, but MCTS move \
                 selection must stay within its configured search budget of (0, {MAX_MCTS_BUDGET}]",
                self.id
            )));
        }
        Ok(())
    }

    /// Determinism invariant: scripted profiles are deterministic for a given
    /// mission and state.
    fn ensure_scripted_determinism(&self) -> Result<(), DomainError> {
        if !self.scripted_determinism_guaranteed {
            return Err(DomainError::InvariantViolation(format!(
                "AI profile '{}' cannot guarantee deterministic scripted move selection; scripted \
                 profiles are deterministic for a given mission and state",
                self.id
            )));
        }
        Ok(())
    }

    /// Tier-mapping invariant for tuning: the profile's already-bound difficulty
    /// tier maps to exactly one strategy kind.
    fn ensure_tier_strategy_mapping_valid(&self) -> Result<(), DomainError> {
        if !self.tier_strategy_mapping_valid {
            return Err(DomainError::InvariantViolation(format!(
                "AI profile '{}' binds a difficulty tier to the wrong strategy kind; a difficulty \
                 tier maps to exactly one strategy kind (scripted for prologue; MCTS for \
                 Standard/Brutal/Legendary)",
                self.id
            )));
        }
        Ok(())
    }

    /// MCTS-budget invariant for tuning: MCTS move selection must stay within its
    /// configured search budget.
    fn ensure_mcts_within_search_budget(&self) -> Result<(), DomainError> {
        if !self.mcts_within_search_budget {
            return Err(DomainError::InvariantViolation(format!(
                "AI profile '{}' exceeds its search budget; MCTS move selection must stay within \
                 its configured search budget",
                self.id
            )));
        }
        Ok(())
    }

    /// Determinism invariant for tuning: scripted profiles are deterministic for
    /// a given mission and state.
    fn ensure_scripted_deterministic(&self) -> Result<(), DomainError> {
        if !self.scripted_deterministic {
            return Err(DomainError::InvariantViolation(format!(
                "AI profile '{}' is not reproducible; scripted profiles are deterministic for a \
                 given mission and state",
                self.id
            )));
        }
        Ok(())
    }

    /// Apply an event to aggregate state.
    fn apply(&mut self, event: &Event) {
        match event {
            Event::AIProfileConfigured(configured) => {
                self.difficulty_tier = Some(configured.difficulty_tier);
                self.strategy_kind = Some(configured.strategy_kind);
                self.mcts_budget = configured.mcts_budget;
            }
            // Tuning adjusts strategy budget/weights; the invariant flags above
            // continue to hold for the newly tuned configuration.
            Event::DifficultyTuned(_) => {}
        }
    }

    /// Handle `ConfigureAIProfileCmd`.
    fn configure_ai_profile(&mut self, cmd: ConfigureAIProfile) -> Result<Vec<Event>, DomainError> {
        if cmd.profile_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "AI profile '{}' requires a valid profileId to configure",
                self.id
            )));
        }
        if cmd.profile_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets AI profile '{}' but this aggregate is AI profile '{}'",
                cmd.profile_id, self.id
            )));
        }

        self.ensure_tier_strategy_mapping(cmd.difficulty_tier, cmd.strategy_kind)?;
        self.ensure_mcts_within_budget(cmd.strategy_kind, cmd.mcts_budget)?;
        self.ensure_scripted_determinism()?;

        let event = Event::AIProfileConfigured(AIProfileConfigured {
            profile_id: cmd.profile_id,
            difficulty_tier: cmd.difficulty_tier,
            strategy_kind: cmd.strategy_kind,
            mcts_budget: cmd.mcts_budget,
        });
        self.apply(&event);
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `TuneDifficultyCmd`.
    fn tune_difficulty(&mut self, cmd: TuneDifficulty) -> Result<Vec<Event>, DomainError> {
        if cmd.profile_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "AI profile '{}' requires a valid profileId to tune difficulty",
                self.id
            )));
        }
        if cmd.tuning_params.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "AI profile '{}' requires valid tuningParams to tune difficulty",
                self.id
            )));
        }
        if cmd.profile_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets AI profile '{}' but this aggregate is AI profile '{}'",
                cmd.profile_id, self.id
            )));
        }

        self.ensure_tier_strategy_mapping_valid()?;
        self.ensure_mcts_within_search_budget()?;
        self.ensure_scripted_deterministic()?;

        let event = Event::DifficultyTuned(DifficultyTuned {
            profile_id: cmd.profile_id,
            tuning_params: cmd.tuning_params,
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
            CONFIGURE_AI_PROFILE => {
                let cmd: ConfigureAIProfile =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ConfigureAIProfileCmd payload: {e}"
                        ))
                    })?;
                self.configure_ai_profile(cmd)
            }
            TUNE_DIFFICULTY => {
                let cmd: TuneDifficulty =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed TuneDifficultyCmd payload: {e}"
                        ))
                    })?;
                self.tune_difficulty(cmd)
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
mod configure_tests {
    use super::*;

    fn ready_profile() -> AIProfile {
        let mut profile = AIProfile::new("profile-01");
        profile.set_scripted_determinism_guaranteed(true);
        profile
    }

    fn valid_cmd() -> ConfigureAIProfile {
        ConfigureAIProfile::new(
            "profile-01",
            DifficultyTier::Standard,
            StrategyKind::Mcts,
            1_200,
        )
    }

    // Scenario: Successfully execute ConfigureAIProfileCmd.
    #[test]
    fn configures_profile_and_emits_event() {
        let mut profile = ready_profile();

        let events = profile
            .execute(valid_cmd().into_command())
            .expect("valid profile configuration should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "ai.profile.configured");
        match &events[0] {
            Event::AIProfileConfigured(configured) => {
                assert_eq!(configured.profile_id, "profile-01");
                assert_eq!(configured.difficulty_tier, DifficultyTier::Standard);
                assert_eq!(configured.strategy_kind, StrategyKind::Mcts);
                assert_eq!(configured.mcts_budget, 1_200);
            }
            other => panic!("expected AIProfileConfigured, got {other:?}"),
        }
        assert_eq!(profile.difficulty_tier(), Some(DifficultyTier::Standard));
        assert_eq!(profile.strategy_kind(), Some(StrategyKind::Mcts));
        assert_eq!(profile.mcts_budget(), 1_200);
        assert_eq!(profile.version(), 1);
        assert_eq!(profile.uncommitted_events().len(), 1);
        assert_eq!(
            profile.uncommitted_events()[0].event_type(),
            "ai.profile.configured"
        );
    }

    // A prologue profile binds to the scripted strategy and configures cleanly.
    #[test]
    fn configures_scripted_prologue_profile() {
        let mut profile = ready_profile();

        let events = profile
            .execute(
                ConfigureAIProfile::new(
                    "profile-01",
                    DifficultyTier::Prologue,
                    StrategyKind::Scripted,
                    0,
                )
                .into_command(),
            )
            .expect("valid scripted prologue configuration should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(profile.strategy_kind(), Some(StrategyKind::Scripted));
        assert_eq!(profile.version(), 1);
    }

    // Scenario: ConfigureAIProfileCmd rejected - A difficulty tier maps to
    // exactly one strategy kind (scripted for prologue; MCTS for
    // Standard/Brutal/Legendary).
    #[test]
    fn rejects_when_tier_maps_to_the_wrong_strategy() {
        let mut profile = ready_profile();

        // Standard is an MCTS tier; binding it to Scripted violates the mapping.
        let err = profile
            .execute(
                ConfigureAIProfile::new(
                    "profile-01",
                    DifficultyTier::Standard,
                    StrategyKind::Scripted,
                    1_200,
                )
                .into_command(),
            )
            .expect_err("a tier bound to the wrong strategy must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.strategy_kind(), None);
        assert_eq!(profile.version(), 0);
    }

    // The prologue tier bound to MCTS is likewise a mapping violation.
    #[test]
    fn rejects_when_prologue_bound_to_mcts() {
        let mut profile = ready_profile();

        let err = profile
            .execute(
                ConfigureAIProfile::new(
                    "profile-01",
                    DifficultyTier::Prologue,
                    StrategyKind::Mcts,
                    1_200,
                )
                .into_command(),
            )
            .expect_err("the scripted prologue tier bound to MCTS must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    // Scenario: ConfigureAIProfileCmd rejected - MCTS move selection must stay
    // within its configured search budget.
    #[test]
    fn rejects_when_mcts_budget_is_zero() {
        let mut profile = ready_profile();

        let err = profile
            .execute(
                ConfigureAIProfile::new(
                    "profile-01",
                    DifficultyTier::Standard,
                    StrategyKind::Mcts,
                    0,
                )
                .into_command(),
            )
            .expect_err("an MCTS profile with no search budget must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    #[test]
    fn rejects_when_mcts_budget_exceeds_ceiling() {
        let mut profile = ready_profile();

        let err = profile
            .execute(
                ConfigureAIProfile::new(
                    "profile-01",
                    DifficultyTier::Legendary,
                    StrategyKind::Mcts,
                    MAX_MCTS_BUDGET + 1,
                )
                .into_command(),
            )
            .expect_err("an MCTS budget beyond the ceiling must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    // Scenario: ConfigureAIProfileCmd rejected - Scripted profiles are
    // deterministic for a given mission and state.
    #[test]
    fn rejects_when_scripted_determinism_cannot_be_guaranteed() {
        let mut profile = ready_profile();
        profile.set_scripted_determinism_guaranteed(false);

        let err = profile
            .execute(valid_cmd().into_command())
            .expect_err("a profile that cannot guarantee scripted determinism must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    #[test]
    fn rejects_missing_profile_id() {
        let mut profile = ready_profile();

        let err = profile
            .execute(
                ConfigureAIProfile::new("   ", DifficultyTier::Standard, StrategyKind::Mcts, 1_200)
                    .into_command(),
            )
            .expect_err("missing profileId must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    #[test]
    fn rejects_command_for_a_different_profile() {
        let mut profile = ready_profile();

        let err = profile
            .execute(
                ConfigureAIProfile::new(
                    "profile-99",
                    DifficultyTier::Standard,
                    StrategyKind::Mcts,
                    1_200,
                )
                .into_command(),
            )
            .expect_err("a command for another profile must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
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

        assert_eq!(command.name, ConfigureAIProfile::COMMAND);
        let decoded: ConfigureAIProfile = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}

#[cfg(test)]
mod tune_tests {
    use super::*;

    fn ready_profile() -> AIProfile {
        let mut profile = AIProfile::new("profile-01");
        profile.set_tier_strategy_mapping_valid(true);
        profile.set_mcts_within_search_budget(true);
        profile.set_scripted_deterministic(true);
        profile
    }

    fn valid_cmd() -> TuneDifficulty {
        TuneDifficulty::new("profile-01", "budget=1500;aggression=0.6")
    }

    // Scenario: Successfully execute TuneDifficultyCmd.
    #[test]
    fn tunes_difficulty_and_emits_event() {
        let mut profile = ready_profile();

        let events = profile
            .execute(valid_cmd().into_command())
            .expect("valid difficulty tuning should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "difficulty.tuned");
        match &events[0] {
            Event::DifficultyTuned(tuned) => {
                assert_eq!(tuned.profile_id, "profile-01");
                assert_eq!(tuned.tuning_params, "budget=1500;aggression=0.6");
            }
            other => panic!("expected DifficultyTuned, got {other:?}"),
        }
        assert_eq!(profile.version(), 1);
        assert_eq!(profile.uncommitted_events().len(), 1);
        assert_eq!(
            profile.uncommitted_events()[0].event_type(),
            "difficulty.tuned"
        );
    }

    // Scenario: TuneDifficultyCmd rejected - A difficulty tier maps to exactly
    // one strategy kind (scripted for prologue; MCTS for Standard/Brutal/Legendary).
    #[test]
    fn rejects_when_tier_strategy_mapping_is_invalid() {
        let mut profile = ready_profile();
        profile.set_tier_strategy_mapping_valid(false);

        let err = profile
            .execute(valid_cmd().into_command())
            .expect_err("a tier mapped to the wrong strategy kind must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    // Scenario: TuneDifficultyCmd rejected - MCTS move selection must stay within
    // its configured search budget.
    #[test]
    fn rejects_when_mcts_exceeds_search_budget() {
        let mut profile = ready_profile();
        profile.set_mcts_within_search_budget(false);

        let err = profile
            .execute(valid_cmd().into_command())
            .expect_err("MCTS selection beyond its search budget must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    // Scenario: TuneDifficultyCmd rejected - Scripted profiles are deterministic
    // for a given mission and state.
    #[test]
    fn rejects_when_scripted_profile_is_not_deterministic() {
        let mut profile = ready_profile();
        profile.set_scripted_deterministic(false);

        let err = profile
            .execute(valid_cmd().into_command())
            .expect_err("a non-deterministic scripted profile must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    #[test]
    fn rejects_command_for_a_different_profile() {
        let mut profile = ready_profile();

        let err = profile
            .execute(TuneDifficulty::new("profile-99", "budget=1500").into_command())
            .expect_err("a tune command for another profile must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    #[test]
    fn rejects_missing_profile_id() {
        let mut profile = ready_profile();

        let err = profile
            .execute(TuneDifficulty::new("   ", "budget=1500").into_command())
            .expect_err("missing profileId must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
    }

    #[test]
    fn rejects_missing_tuning_params() {
        let mut profile = ready_profile();

        let err = profile
            .execute(TuneDifficulty::new("profile-01", "   ").into_command())
            .expect_err("missing tuningParams must be rejected");

        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(profile.version(), 0);
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

        assert_eq!(command.name, TuneDifficulty::COMMAND);
        let decoded: TuneDifficulty = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
