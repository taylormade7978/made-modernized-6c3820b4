//! ExpansionSet bounded context — released card sets/expansions and their cards.
//!
//! An [`ExpansionSet`] is the catalog entry for one publishable expansion: its
//! set code, the cards it ships, the keywords it introduces, and the release
//! state that gates publication. Three invariants keep an expansion releasable:
//!
//! 1. **Validator passed** — an expansion cannot be released until it passes the
//!    validator (schema, cost/class legality, copy caps).
//! 2. **Keywords registered** — any new keyword introduced by the set must be
//!    registered and implemented in the engine before release.
//! 3. **Legal card count** — the set's card count must fall within the declared
//!    bounds for an expansion ([`DECLARED_CARD_COUNT_BOUNDS`]).
//!
//! Two commands are implemented:
//!
//! - [`ValidateExpansion`] (`ValidateExpansionCmd`): runs the validator over the
//!   draft set. It enforces all three release gates and, on success, emits
//!   [`Event::ExpansionValidated`] (`expansion.validated`) — the signal that the
//!   draft is releasable.
//! - [`ReleaseExpansion`] (`ReleaseExpansionCmd`): publishes the validated set to
//!   a release channel, enforcing all three invariants, and on success emits
//!   [`Event::ExpansionReleased`] (`expansion.released`).
//!
//! This module is hand-written (it no longer uses `shared::stub_aggregate!`) but
//! preserves the same public surface — an [`ExpansionSet`] aggregate and an
//! [`ExpansionSetRepository`] port — so the persistence adapters in
//! `crates/mocks` keep compiling unchanged.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "ExpansionSet";

/// The command names [`ExpansionSet::execute`] recognizes.
const VALIDATE_EXPANSION: &str = "ValidateExpansionCmd";
const RELEASE_EXPANSION: &str = "ReleaseExpansionCmd";

/// The inclusive `[min, max]` legal range for the number of cards an expansion
/// may ship. A release-valid set must sit inside these authored bounds. Modeling
/// the range as domain-visible knowledge keeps the aggregate self-contained and
/// testable, mirroring the HP/cost ranges the other catalog contexts carry.
pub const DECLARED_CARD_COUNT_BOUNDS: std::ops::RangeInclusive<usize> = 100..=350;

/// The `ValidateExpansionCmd` payload: the set code of the draft to validate.
/// Field names are the catalog's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ValidateExpansion::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`ExpansionSet::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateExpansion {
    /// Set code of the expansion being validated; must name the expansion this
    /// aggregate records.
    pub set_code: String,
}

impl ValidateExpansion {
    /// The command name this maps to.
    pub const COMMAND: &'static str = VALIDATE_EXPANSION;

    /// Build a command validating the draft expansion `set_code`.
    pub fn new(set_code: impl Into<String>) -> Self {
        Self {
            set_code: set_code.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`ExpansionSet::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ValidateExpansion is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `ReleaseExpansionCmd` payload: the set to publish and the channel to
/// publish it to. Field names are the catalog's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ReleaseExpansion::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`ExpansionSet::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseExpansion {
    /// Set code of the expansion being released; must name the expansion this
    /// aggregate records.
    pub set_code: String,
    /// The release channel to publish to (e.g. `"live"`, `"beta"`). Must be
    /// non-empty for a release to be considered valid.
    pub release_channel: String,
}

impl ReleaseExpansion {
    /// The command name this maps to.
    pub const COMMAND: &'static str = RELEASE_EXPANSION;

    /// Build a command releasing expansion `set_code` to `release_channel`.
    pub fn new(set_code: impl Into<String>, release_channel: impl Into<String>) -> Self {
        Self {
            set_code: set_code.into(),
            release_channel: release_channel.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`ExpansionSet::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ReleaseExpansion is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The validated expansion, carried by [`Event::ExpansionValidated`] and thus by
/// the emitted `expansion.validated` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpansionValidated {
    /// The set code that passed the validator.
    pub set_code: String,
}

/// The published expansion, carried by [`Event::ExpansionReleased`] and thus by
/// the emitted `expansion.released` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpansionReleased {
    /// The set code that was published.
    pub set_code: String,
    /// The channel the set was published to.
    pub release_channel: String,
}

/// Domain events emitted by [`ExpansionSet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A draft expansion cleared the validator and every release gate.
    ExpansionValidated(ExpansionValidated),
    /// An expansion passed every invariant and was published to a channel.
    ExpansionReleased(ExpansionReleased),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::ExpansionValidated(_) => "expansion.validated",
            Event::ExpansionReleased(_) => "expansion.released",
        }
    }
}

/// The ExpansionSet aggregate: the catalog entry for a single publishable
/// expansion.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the server — is unchanged, while it now carries the
/// expansion's release-gating state: its set code, whether it passed the
/// validator, the keywords it introduces and the keywords the engine has
/// registered, and the count of cards it ships. Its `execute` handles
/// [`ReleaseExpansionCmd`].
///
/// A fresh expansion from [`ExpansionSet::new`] is intentionally *not
/// releasable* (validator not passed, no cards, no registered keywords); the
/// configuration methods below bring it up to a release-valid shape, exactly as
/// [`BossDefinition`](crate::boss_definition) is built up before a command
/// validates it.
#[derive(Debug)]
pub struct ExpansionSet {
    id: String,
    root: AggregateRoot,
    /// Set code of the expansion this aggregate records. A `ReleaseExpansionCmd`
    /// must name this same set code.
    set_code: String,
    /// Whether the set has passed the validator (schema, cost/class legality,
    /// copy caps). Must be `true` before a release is allowed.
    validator_passed: bool,
    /// The keywords this set introduces. Every one must appear in
    /// `registered_keywords` before release.
    introduced_keywords: BTreeSet<String>,
    /// The keywords registered and implemented in the engine.
    registered_keywords: BTreeSet<String>,
    /// The number of cards this set ships; must fall within
    /// [`DECLARED_CARD_COUNT_BOUNDS`].
    card_count: usize,
}

impl ExpansionSet {
    /// Create a new, not-yet-releasable expansion identified by `id`. Use the
    /// configuration methods to bring it to a release-valid shape before
    /// releasing it.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            set_code: id.clone(),
            id,
            root: AggregateRoot::new(),
            validator_passed: false,
            introduced_keywords: BTreeSet::new(),
            registered_keywords: BTreeSet::new(),
            card_count: 0,
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The set code this aggregate records.
    pub fn set_code(&self) -> &str {
        &self.set_code
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// Record the outcome of running the validator (schema, cost/class legality,
    /// copy caps) over the set.
    pub fn set_validator_passed(&mut self, passed: bool) {
        self.validator_passed = passed;
    }

    /// Declare the keywords this set introduces (and therefore must have
    /// registered in the engine before release).
    pub fn introduce_keywords<I, S>(&mut self, keywords: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.introduced_keywords = keywords.into_iter().map(Into::into).collect();
    }

    /// Register the keywords the engine has implemented. A release requires every
    /// introduced keyword to appear here.
    pub fn register_keywords<I, S>(&mut self, keywords: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.registered_keywords = keywords.into_iter().map(Into::into).collect();
    }

    /// Set the number of cards this set ships.
    pub fn set_card_count(&mut self, count: usize) {
        self.card_count = count;
    }

    /// Validator invariant: an expansion cannot be released until it passes the
    /// validator (schema, cost/class legality, copy caps).
    fn ensure_validator_passed(&self) -> Result<(), DomainError> {
        if !self.validator_passed {
            return Err(DomainError::InvariantViolation(format!(
                "expansion '{}' cannot be released until it passes the validator (schema, \
                 cost/class legality, copy caps)",
                self.set_code
            )));
        }
        Ok(())
    }

    /// Keyword-registration invariant: every keyword introduced by the set must
    /// be registered and implemented in the engine before release.
    fn ensure_keywords_registered(&self) -> Result<(), DomainError> {
        for keyword in &self.introduced_keywords {
            if !self.registered_keywords.contains(keyword) {
                return Err(DomainError::InvariantViolation(format!(
                    "keyword '{keyword}' introduced by expansion '{}' must be registered and \
                     implemented in the engine before release",
                    self.set_code
                )));
            }
        }
        Ok(())
    }

    /// Card-count invariant: the set's card count must fall within the declared
    /// bounds for an expansion.
    fn ensure_card_count_within_bounds(&self) -> Result<(), DomainError> {
        if !DECLARED_CARD_COUNT_BOUNDS.contains(&self.card_count) {
            return Err(DomainError::InvariantViolation(format!(
                "expansion card count must fall within [{}, {}]; expansion '{}' ships {}",
                DECLARED_CARD_COUNT_BOUNDS.start(),
                DECLARED_CARD_COUNT_BOUNDS.end(),
                self.set_code,
                self.card_count
            )));
        }
        Ok(())
    }

    /// Handle `ValidateExpansionCmd`: run the validator over the draft set.
    /// Verify the command targets this expansion, enforce all three release
    /// gates (validator passed, keywords registered, card count within bounds),
    /// and emit [`Event::ExpansionValidated`].
    fn validate_expansion(&mut self, cmd: ValidateExpansion) -> Result<Vec<Event>, DomainError> {
        // The command must name the expansion this aggregate actually records.
        if cmd.set_code != self.set_code {
            return Err(DomainError::InvariantViolation(format!(
                "command targets expansion '{}' but this aggregate records '{}'",
                cmd.set_code, self.set_code
            )));
        }

        // Running the validator means enforcing every release gate over the draft.
        self.ensure_validator_passed()?;
        self.ensure_keywords_registered()?;
        self.ensure_card_count_within_bounds()?;

        let event = Event::ExpansionValidated(ExpansionValidated {
            set_code: cmd.set_code,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `ReleaseExpansionCmd`: verify the command targets this expansion
    /// and carries a valid channel, enforce all three release invariants, and
    /// emit [`Event::ExpansionReleased`].
    fn release_expansion(&mut self, cmd: ReleaseExpansion) -> Result<Vec<Event>, DomainError> {
        // The command must name the expansion this aggregate actually records.
        if cmd.set_code != self.set_code {
            return Err(DomainError::InvariantViolation(format!(
                "command targets expansion '{}' but this aggregate records '{}'",
                cmd.set_code, self.set_code
            )));
        }
        // A release must name a channel to publish to.
        if cmd.release_channel.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "expansion '{}' must be released to a non-empty release channel",
                self.set_code
            )));
        }

        // Enforce every release invariant before publishing anything.
        self.ensure_validator_passed()?;
        self.ensure_keywords_registered()?;
        self.ensure_card_count_within_bounds()?;

        let event = Event::ExpansionReleased(ExpansionReleased {
            set_code: cmd.set_code,
            release_channel: cmd.release_channel,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for ExpansionSet {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            VALIDATE_EXPANSION => {
                let cmd: ValidateExpansion =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ValidateExpansionCmd payload: {e}"
                        ))
                    })?;
                self.validate_expansion(cmd)
            }
            RELEASE_EXPANSION => {
                let cmd: ReleaseExpansion =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ReleaseExpansionCmd payload: {e}"
                        ))
                    })?;
                self.release_expansion(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`ExpansionSet`] aggregate. Adapters implement
/// [`shared::Repository`] for `ExpansionSet` and then this marker trait.
pub trait ExpansionSetRepository: Repository<ExpansionSet> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A release-valid expansion `SET-01`: passed the validator, every
    /// introduced keyword is registered, and a card count within the declared
    /// bounds. Tests mutate one aspect at a time to drive a specific rejection.
    fn valid_expansion() -> ExpansionSet {
        let mut set = ExpansionSet::new("SET-01");
        set.set_validator_passed(true);
        set.introduce_keywords(["Overload", "Rush"]);
        set.register_keywords(["Overload", "Rush", "Taunt"]);
        set.set_card_count(135);
        set
    }

    /// A command releasing `SET-01` to the `live` channel.
    fn valid_cmd() -> ReleaseExpansion {
        ReleaseExpansion::new("SET-01", "live")
    }

    // ---- ValidateExpansionCmd (S-20) ----

    // Scenario: Successfully execute ValidateExpansionCmd.
    #[test]
    fn validates_expansion_and_emits_expansion_validated_event() {
        let mut set = valid_expansion();

        let events = set
            .execute(ValidateExpansion::new("SET-01").into_command())
            .expect("valid validation should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "expansion.validated");
        match &events[0] {
            Event::ExpansionValidated(validated) => {
                assert_eq!(validated.set_code, "SET-01");
            }
            other => panic!("expected ExpansionValidated, got {other:?}"),
        }
        // The event was recorded on the aggregate root.
        assert_eq!(set.version(), 1);
        assert_eq!(set.uncommitted_events().len(), 1);
        assert_eq!(
            set.uncommitted_events()[0].event_type(),
            "expansion.validated"
        );
    }

    // Scenario: ValidateExpansionCmd rejected — an expansion cannot be released
    // until it passes the validator (schema, cost/class legality, copy caps).
    #[test]
    fn validate_rejects_when_validator_has_not_passed() {
        let mut set = valid_expansion();
        // The draft has not cleared the schema/cost/class validator.
        set.set_validator_passed(false);

        let err = set
            .execute(ValidateExpansion::new("SET-01").into_command())
            .expect_err("an unvalidated expansion must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: ValidateExpansionCmd rejected — any new keyword introduced by the
    // set must be registered and implemented in the engine before release.
    #[test]
    fn validate_rejects_when_introduced_keyword_is_not_registered() {
        let mut set = valid_expansion();
        // Introduces a keyword the engine has not registered.
        set.introduce_keywords(["Overload", "Corrupt"]);

        let err = set
            .execute(ValidateExpansion::new("SET-01").into_command())
            .expect_err("an unregistered keyword must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: ValidateExpansionCmd rejected — set card count must fall within
    // the declared bounds for an expansion (below the minimum).
    #[test]
    fn validate_rejects_when_card_count_below_declared_bounds() {
        let mut set = valid_expansion();
        // One short of the declared minimum.
        set.set_card_count(DECLARED_CARD_COUNT_BOUNDS.start() - 1);

        let err = set
            .execute(ValidateExpansion::new("SET-01").into_command())
            .expect_err("a card count below the bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: ValidateExpansionCmd rejected — set card count must fall within
    // the declared bounds for an expansion (above the maximum).
    #[test]
    fn validate_rejects_when_card_count_above_declared_bounds() {
        let mut set = valid_expansion();
        // One past the declared maximum.
        set.set_card_count(DECLARED_CARD_COUNT_BOUNDS.end() + 1);

        let err = set
            .execute(ValidateExpansion::new("SET-01").into_command())
            .expect_err("a card count above the bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // A ValidateExpansionCmd must name the expansion this aggregate records.
    #[test]
    fn validate_rejects_when_command_targets_a_different_set() {
        let mut set = valid_expansion();

        let err = set
            .execute(ValidateExpansion::new("SET-99").into_command())
            .expect_err("a mismatched set code must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    #[test]
    fn validate_command_payload_round_trips() {
        let cmd = ValidateExpansion::new("SET-01");
        let command = cmd.into_command();
        assert_eq!(command.name, ValidateExpansion::COMMAND);
        let decoded: ValidateExpansion = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, ValidateExpansion::new("SET-01"));
    }

    // Scenario: Successfully execute ReleaseExpansionCmd.
    #[test]
    fn releases_expansion_and_emits_expansion_released_event() {
        let mut set = valid_expansion();

        let events = set
            .execute(valid_cmd().into_command())
            .expect("valid release should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "expansion.released");
        match &events[0] {
            Event::ExpansionReleased(released) => {
                assert_eq!(released.set_code, "SET-01");
                assert_eq!(released.release_channel, "live");
            }
            other => panic!("expected ExpansionReleased, got {other:?}"),
        }
        // The event was recorded on the aggregate root.
        assert_eq!(set.version(), 1);
        assert_eq!(set.uncommitted_events().len(), 1);
        assert_eq!(
            set.uncommitted_events()[0].event_type(),
            "expansion.released"
        );
    }

    // Scenario: rejected — an expansion cannot be released until it passes the
    // validator (schema, cost/class legality, copy caps).
    #[test]
    fn rejects_when_validator_has_not_passed() {
        let mut set = valid_expansion();
        // The set has not cleared the validator.
        set.set_validator_passed(false);

        let err = set
            .execute(valid_cmd().into_command())
            .expect_err("an unvalidated expansion must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: rejected — any new keyword introduced by the set must be
    // registered and implemented in the engine before release.
    #[test]
    fn rejects_when_introduced_keyword_is_not_registered() {
        let mut set = valid_expansion();
        // Introduces a keyword the engine has not registered.
        set.introduce_keywords(["Overload", "Corrupt"]);

        let err = set
            .execute(valid_cmd().into_command())
            .expect_err("an unregistered keyword must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: rejected — set card count must fall within the declared bounds
    // for an expansion (below the minimum).
    #[test]
    fn rejects_when_card_count_below_declared_bounds() {
        let mut set = valid_expansion();
        // One short of the declared minimum.
        set.set_card_count(DECLARED_CARD_COUNT_BOUNDS.start() - 1);

        let err = set
            .execute(valid_cmd().into_command())
            .expect_err("a card count below the bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: rejected — set card count must fall within the declared bounds
    // for an expansion (above the maximum).
    #[test]
    fn rejects_when_card_count_above_declared_bounds() {
        let mut set = valid_expansion();
        // One past the declared maximum.
        set.set_card_count(DECLARED_CARD_COUNT_BOUNDS.end() + 1);

        let err = set
            .execute(valid_cmd().into_command())
            .expect_err("a card count above the bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // A release must name a channel to publish to.
    #[test]
    fn rejects_empty_release_channel() {
        let mut set = valid_expansion();
        let cmd = ReleaseExpansion::new("SET-01", "   ");

        let err = set
            .execute(cmd.into_command())
            .expect_err("an empty release channel must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut set = ExpansionSet::new("SET-01");
        let err = set.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "ExpansionSet");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, ReleaseExpansion::COMMAND);
        let decoded: ReleaseExpansion = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
