//! BossDefinition bounded context — PvE boss encounters and their rules.
//!
//! A [`BossDefinition`] is the catalog entry for one of the game's authoritative
//! Bosses: its identity, the single hero power and single trademark that make it
//! recognizable, its starting HP, and — once bound — the set of signature cards
//! it plays. Four invariants keep a Boss well-formed:
//!
//! 1. **Signature shape** — every Boss has *exactly one* hero power and *exactly
//!    one* trademark; neither may be missing or doubled.
//! 2. **Signature set** — every Boss must be assigned a *non-empty* signature
//!    card set, and every card in it must be drawn from a valid
//!    [`CardDefinition`](crate::card_definition) already in the catalog.
//! 3. **Legal HP** — a Boss's starting HP must fall within the legal HP bounds
//!    ([`LEGAL_STARTING_HP`]).
//! 4. **Authoritative roster** — the published roster must contain *exactly*
//!    [`AUTHORITATIVE_ROSTER_SIZE`] Bosses before a launch is validated.
//!
//! Two commands are implemented. [`AssignSignatureCards`]
//! (`AssignSignatureCardsCmd`) binds a Boss's signature card set from valid
//! card definitions, enforcing all four invariants, and on success emits
//! [`Event::SignatureAssigned`] (`boss.signature.assigned`).
//! [`ValidateBossRoster`] (`ValidateBossRosterCmd`) asserts the full 18-Boss
//! roster is complete and legal: it re-checks the same four invariants against
//! the Boss's *standing* state (including its already-bound signature set) and,
//! on success, emits [`Event::RosterValidated`] (`boss.roster.validated`). This
//! module is hand-written (it no longer uses `shared::stub_aggregate!`) but
//! preserves the same public surface — a [`BossDefinition`] aggregate and a
//! [`BossDefinitionRepository`] port — so the persistence adapters in
//! `crates/mocks` keep compiling unchanged.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "BossDefinition";

/// The command names [`BossDefinition::execute`] recognizes.
const ASSIGN_SIGNATURE_CARDS: &str = "AssignSignatureCardsCmd";
const VALIDATE_BOSS_ROSTER: &str = "ValidateBossRosterCmd";

/// The inclusive `[min, max]` legal range for a Boss's starting HP. Bosses are
/// beefier than players, but a launch-valid Boss must still sit inside these
/// authored bounds. Modeling the range as domain-visible knowledge keeps the
/// aggregate self-contained and testable, mirroring the cost ranges the
/// [`CardDefinition`](crate::card_definition) context carries.
pub const LEGAL_STARTING_HP: std::ops::RangeInclusive<i64> = 30..=90;

/// The authoritative roster size: the published roster must contain exactly this
/// many Bosses before a launch is validated.
pub const AUTHORITATIVE_ROSTER_SIZE: usize = 18;

/// The `AssignSignatureCardsCmd` payload: the Boss to bind and the signature
/// card set to bind to it. Field names are the catalog's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`AssignSignatureCards::into_command`], or decode it from a command payload
/// via [`serde_json`] inside [`BossDefinition::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignSignatureCards {
    /// Identity of the Boss whose signature set is being bound; must name the
    /// Boss this aggregate records.
    pub boss_id: String,
    /// The signature card set to bind. Must be non-empty and every id must
    /// resolve to a valid card definition in the catalog.
    pub signature_card_ids: Vec<String>,
}

impl AssignSignatureCards {
    /// The command name this maps to.
    pub const COMMAND: &'static str = ASSIGN_SIGNATURE_CARDS;

    /// Build a command binding `signature_card_ids` to Boss `boss_id`.
    pub fn new(boss_id: impl Into<String>, signature_card_ids: Vec<String>) -> Self {
        Self {
            boss_id: boss_id.into(),
            signature_card_ids,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`BossDefinition::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload =
            serde_json::to_vec(self).expect("AssignSignatureCards is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `ValidateBossRosterCmd` payload: the roster being launch-validated and
/// the number of Bosses it is expected to contain. Field names are the catalog's
/// `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ValidateBossRoster::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`BossDefinition::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateBossRoster {
    /// Identity of the roster being validated for launch.
    pub roster_id: String,
    /// The number of Bosses the roster is expected to hold; must equal the
    /// authoritative [`AUTHORITATIVE_ROSTER_SIZE`].
    pub expected_count: usize,
}

impl ValidateBossRoster {
    /// The command name this maps to.
    pub const COMMAND: &'static str = VALIDATE_BOSS_ROSTER;

    /// Build a command asserting `roster_id` should hold `expected_count` Bosses.
    pub fn new(roster_id: impl Into<String>, expected_count: usize) -> Self {
        Self {
            roster_id: roster_id.into(),
            expected_count,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`BossDefinition::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ValidateBossRoster is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The signature card set proven-valid for a Boss, carried by
/// [`Event::SignatureAssigned`] and thus by the emitted `boss.signature.assigned`
/// event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureAssigned {
    /// The Boss the signature set was bound to.
    pub boss_id: String,
    /// The bound signature card ids, in the order supplied.
    pub signature_card_ids: Vec<String>,
}

/// The proof that a Boss's roster is complete and legal, carried by
/// [`Event::RosterValidated`] and thus by the emitted `boss.roster.validated`
/// event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterValidated {
    /// The roster that passed launch validation.
    pub roster_id: String,
    /// The Boss whose standing state was validated against the roster.
    pub boss_id: String,
    /// The number of Bosses confirmed present on the validated roster.
    pub validated_count: usize,
}

/// Domain events emitted by [`BossDefinition`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A Boss's signature card set passed every invariant and was bound.
    SignatureAssigned(SignatureAssigned),
    /// A Boss's roster passed every invariant and was launch-validated.
    RosterValidated(RosterValidated),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::SignatureAssigned(_) => "boss.signature.assigned",
            Event::RosterValidated(_) => "boss.roster.validated",
        }
    }
}

/// The BossDefinition aggregate: the catalog entry for a single authoritative
/// Boss.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the server — is unchanged, while it now carries the
/// Boss's shape (hero powers, trademarks, starting HP), the catalog of card ids
/// its signature set may draw from, and the published roster size, and its
/// `execute` handles [`AssignSignatureCardsCmd`].
///
/// A fresh Boss from [`BossDefinition::new`] is intentionally *incomplete* (no
/// hero power, no trademark, zero HP, empty catalog, empty roster); the
/// configuration methods below bring it up to a launch-valid shape, exactly as
/// [`MatchReplay`](crate::match_replay) is built up before a command validates
/// it.
#[derive(Debug)]
pub struct BossDefinition {
    id: String,
    root: AggregateRoot,
    /// Identity of the Boss this aggregate records. An `AssignSignatureCardsCmd`
    /// must name this same Boss.
    boss_id: String,
    /// The hero powers declared on the Boss. Exactly one is legal.
    hero_powers: Vec<String>,
    /// The trademarks declared on the Boss. Exactly one is legal.
    trademarks: Vec<String>,
    /// The Boss's starting HP; must fall within [`LEGAL_STARTING_HP`].
    starting_hp: i64,
    /// The catalog of valid [`CardDefinition`](crate::card_definition) ids a
    /// signature card must be drawn from.
    valid_card_ids: BTreeSet<String>,
    /// The signature card set currently bound to the Boss (empty until an
    /// `AssignSignatureCardsCmd` succeeds). Roster validation checks this
    /// standing set against the signature-set invariant.
    signature_card_ids: Vec<String>,
    /// The number of Bosses on the published roster; must equal
    /// [`AUTHORITATIVE_ROSTER_SIZE`] for a launch to validate.
    published_roster_size: usize,
}

impl BossDefinition {
    /// Create a new, incomplete Boss identified by `id`. Use the configuration
    /// methods to bring it to a launch-valid shape before assigning a signature
    /// set.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            boss_id: id.clone(),
            id,
            root: AggregateRoot::new(),
            hero_powers: Vec::new(),
            trademarks: Vec::new(),
            starting_hp: 0,
            valid_card_ids: BTreeSet::new(),
            signature_card_ids: Vec::new(),
            published_roster_size: 0,
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The Boss this aggregate records.
    pub fn boss_id(&self) -> &str {
        &self.boss_id
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// Declare the Boss's single hero power. Calling this more than once leaves
    /// the Boss with multiple hero powers, which the signature-shape invariant
    /// rejects.
    pub fn declare_hero_power(&mut self, name: impl Into<String>) {
        self.hero_powers.push(name.into());
    }

    /// Declare the Boss's single trademark. Calling this more than once leaves
    /// the Boss with multiple trademarks, which the signature-shape invariant
    /// rejects.
    pub fn declare_trademark(&mut self, name: impl Into<String>) {
        self.trademarks.push(name.into());
    }

    /// Set the Boss's starting HP.
    pub fn set_starting_hp(&mut self, hp: i64) {
        self.starting_hp = hp;
    }

    /// Register the catalog of valid card ids a signature card may be drawn from.
    pub fn register_valid_cards<I, S>(&mut self, ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.valid_card_ids = ids.into_iter().map(Into::into).collect();
    }

    /// Bind the Boss's standing signature card set directly (mirrors the effect
    /// of a successful `AssignSignatureCardsCmd`). Useful for bringing a Boss to
    /// a launch-valid shape before a `ValidateBossRosterCmd`.
    pub fn assign_signature_set<I, S>(&mut self, ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.signature_card_ids = ids.into_iter().map(Into::into).collect();
    }

    /// Set the size of the published roster this Boss belongs to.
    pub fn set_published_roster_size(&mut self, size: usize) {
        self.published_roster_size = size;
    }

    /// Signature-shape invariant: exactly one hero power and exactly one
    /// trademark.
    fn ensure_signature_shape(&self) -> Result<(), DomainError> {
        if self.hero_powers.len() != 1 {
            return Err(DomainError::InvariantViolation(format!(
                "every Boss has exactly one hero power; Boss '{}' declares {}",
                self.boss_id,
                self.hero_powers.len()
            )));
        }
        if self.trademarks.len() != 1 {
            return Err(DomainError::InvariantViolation(format!(
                "every Boss has exactly one trademark; Boss '{}' declares {}",
                self.boss_id,
                self.trademarks.len()
            )));
        }
        Ok(())
    }

    /// Legal-HP invariant: starting HP falls within [`LEGAL_STARTING_HP`].
    fn ensure_legal_hp(&self) -> Result<(), DomainError> {
        if !LEGAL_STARTING_HP.contains(&self.starting_hp) {
            return Err(DomainError::InvariantViolation(format!(
                "Boss starting HP must fall within [{}, {}]; Boss '{}' has {}",
                LEGAL_STARTING_HP.start(),
                LEGAL_STARTING_HP.end(),
                self.boss_id,
                self.starting_hp
            )));
        }
        Ok(())
    }

    /// Authoritative-roster invariant: the published roster holds exactly
    /// [`AUTHORITATIVE_ROSTER_SIZE`] Bosses.
    fn ensure_authoritative_roster(&self) -> Result<(), DomainError> {
        if self.published_roster_size != AUTHORITATIVE_ROSTER_SIZE {
            return Err(DomainError::InvariantViolation(format!(
                "the published roster must contain exactly {AUTHORITATIVE_ROSTER_SIZE} Bosses \
                 before a launch is validated; found {}",
                self.published_roster_size
            )));
        }
        Ok(())
    }

    /// Signature-set invariant: the set is non-empty and every card id resolves
    /// to a valid card definition in the catalog.
    fn ensure_signature_set(&self, signature_card_ids: &[String]) -> Result<(), DomainError> {
        if signature_card_ids.is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "every Boss must be assigned a non-empty signature card set; Boss '{}' was given \
                 none",
                self.boss_id
            )));
        }
        for card_id in signature_card_ids {
            if !self.valid_card_ids.contains(card_id) {
                return Err(DomainError::InvariantViolation(format!(
                    "signature card '{card_id}' is not drawn from a valid CardDefinition"
                )));
            }
        }
        Ok(())
    }

    /// Handle `AssignSignatureCardsCmd`: verify the command targets this Boss,
    /// enforce all four invariants, bind the signature set, and emit
    /// [`Event::SignatureAssigned`].
    fn assign_signature_cards(
        &mut self,
        cmd: AssignSignatureCards,
    ) -> Result<Vec<Event>, DomainError> {
        // The command must name the Boss this aggregate actually records.
        if cmd.boss_id != self.boss_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets Boss '{}' but this aggregate records '{}'",
                cmd.boss_id, self.boss_id
            )));
        }

        // Enforce the standing shape of the Boss before binding anything.
        self.ensure_signature_shape()?;
        self.ensure_legal_hp()?;
        self.ensure_authoritative_roster()?;
        // Then the invariant carried by the command itself.
        self.ensure_signature_set(&cmd.signature_card_ids)?;

        // Bind the proven-valid set as standing state so a later
        // ValidateBossRosterCmd can re-check the signature-set invariant.
        self.signature_card_ids = cmd.signature_card_ids.clone();

        let event = Event::SignatureAssigned(SignatureAssigned {
            boss_id: cmd.boss_id,
            signature_card_ids: cmd.signature_card_ids,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `ValidateBossRosterCmd`: assert the full 18-Boss roster is complete
    /// and legal by re-checking all four invariants against the Boss's standing
    /// state, confirm the command's `expected_count` agrees with the
    /// authoritative roster size, and emit [`Event::RosterValidated`].
    fn validate_boss_roster(&mut self, cmd: ValidateBossRoster) -> Result<Vec<Event>, DomainError> {
        // The count the command expects must be the authoritative roster size.
        if cmd.expected_count != AUTHORITATIVE_ROSTER_SIZE {
            return Err(DomainError::InvariantViolation(format!(
                "ValidateBossRosterCmd expects {} Bosses but the authoritative roster size is \
                 {AUTHORITATIVE_ROSTER_SIZE}",
                cmd.expected_count
            )));
        }

        // Every standing invariant must hold before a launch is validated.
        self.ensure_signature_shape()?;
        self.ensure_signature_set(&self.signature_card_ids)?;
        self.ensure_legal_hp()?;
        self.ensure_authoritative_roster()?;

        let event = Event::RosterValidated(RosterValidated {
            roster_id: cmd.roster_id,
            boss_id: self.boss_id.clone(),
            validated_count: self.published_roster_size,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for BossDefinition {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            ASSIGN_SIGNATURE_CARDS => {
                let cmd: AssignSignatureCards =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed AssignSignatureCardsCmd payload: {e}"
                        ))
                    })?;
                self.assign_signature_cards(cmd)
            }
            VALIDATE_BOSS_ROSTER => {
                let cmd: ValidateBossRoster =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ValidateBossRosterCmd payload: {e}"
                        ))
                    })?;
                self.validate_boss_roster(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`BossDefinition`] aggregate. Adapters implement
/// [`shared::Repository`] for `BossDefinition` and then this marker trait.
pub trait BossDefinitionRepository: Repository<BossDefinition> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A launch-valid Boss `boss-01`: exactly one hero power and trademark,
    /// legal HP, a catalog of three valid signature cards, and a full 18-Boss
    /// published roster. Tests mutate one aspect at a time to drive a specific
    /// rejection.
    fn valid_boss() -> BossDefinition {
        let mut boss = BossDefinition::new("boss-01");
        boss.declare_hero_power("Smash and Grab");
        boss.declare_trademark("The Vault Door");
        boss.set_starting_hp(60);
        boss.register_valid_cards(["card-001", "card-002", "card-003"]);
        boss.assign_signature_set(["card-001", "card-002"]);
        boss.set_published_roster_size(AUTHORITATIVE_ROSTER_SIZE);
        boss
    }

    /// A command validating the authoritative roster `roster-2026`.
    fn valid_roster_cmd() -> ValidateBossRoster {
        ValidateBossRoster::new("roster-2026", AUTHORITATIVE_ROSTER_SIZE)
    }

    /// A command binding two valid signature cards to `boss-01`.
    fn valid_cmd() -> AssignSignatureCards {
        AssignSignatureCards::new(
            "boss-01",
            vec!["card-001".to_string(), "card-002".to_string()],
        )
    }

    // Scenario: Successfully execute AssignSignatureCardsCmd.
    #[test]
    fn assigns_signature_and_emits_boss_signature_assigned_event() {
        let mut boss = valid_boss();

        let events = boss
            .execute(valid_cmd().into_command())
            .expect("valid assignment should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "boss.signature.assigned");
        match &events[0] {
            Event::SignatureAssigned(assigned) => {
                assert_eq!(assigned.boss_id, "boss-01");
                assert_eq!(assigned.signature_card_ids, vec!["card-001", "card-002"]);
            }
            other => panic!("expected SignatureAssigned, got {other:?}"),
        }
        // The event was recorded on the aggregate root.
        assert_eq!(boss.version(), 1);
        assert_eq!(boss.uncommitted_events().len(), 1);
        assert_eq!(
            boss.uncommitted_events()[0].event_type(),
            "boss.signature.assigned"
        );
    }

    // Scenario: rejected — every Boss has exactly one hero power and exactly one
    // trademark.
    #[test]
    fn rejects_when_boss_has_more_than_one_hero_power() {
        let mut boss = valid_boss();
        // A second hero power breaks the exactly-one shape.
        boss.declare_hero_power("Second Wind");

        let err = boss
            .execute(valid_cmd().into_command())
            .expect_err("a Boss with two hero powers must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(boss.version(), 0);
    }

    // Scenario: rejected — every Boss must be assigned a non-empty signature card
    // set drawn from valid CardDefinitions.
    #[test]
    fn rejects_empty_signature_card_set() {
        let mut boss = valid_boss();
        let cmd = AssignSignatureCards::new("boss-01", Vec::new());

        let err = boss
            .execute(cmd.into_command())
            .expect_err("an empty signature set must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(boss.version(), 0);
    }

    // Scenario: rejected — signature cards must be drawn from valid
    // CardDefinitions (an id outside the catalog).
    #[test]
    fn rejects_signature_card_not_in_catalog() {
        let mut boss = valid_boss();
        let cmd = AssignSignatureCards::new(
            "boss-01",
            vec!["card-001".to_string(), "card-999".to_string()],
        );

        let err = boss
            .execute(cmd.into_command())
            .expect_err("a card outside the catalog must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: rejected — Boss starting HP must fall within the legal HP bounds.
    #[test]
    fn rejects_starting_hp_outside_legal_bounds() {
        let mut boss = valid_boss();
        // One past the legal maximum.
        boss.set_starting_hp(LEGAL_STARTING_HP.end() + 1);

        let err = boss
            .execute(valid_cmd().into_command())
            .expect_err("HP outside the legal bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: rejected — the published roster must contain exactly the
    // authoritative 18 Bosses before a launch is validated.
    #[test]
    fn rejects_when_published_roster_is_not_authoritative_size() {
        let mut boss = valid_boss();
        // One short of the authoritative roster.
        boss.set_published_roster_size(AUTHORITATIVE_ROSTER_SIZE - 1);

        let err = boss
            .execute(valid_cmd().into_command())
            .expect_err("an incomplete roster must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: Successfully execute ValidateBossRosterCmd.
    #[test]
    fn validates_roster_and_emits_boss_roster_validated_event() {
        let mut boss = valid_boss();

        let events = boss
            .execute(valid_roster_cmd().into_command())
            .expect("a complete, legal roster should validate");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "boss.roster.validated");
        match &events[0] {
            Event::RosterValidated(validated) => {
                assert_eq!(validated.roster_id, "roster-2026");
                assert_eq!(validated.boss_id, "boss-01");
                assert_eq!(validated.validated_count, AUTHORITATIVE_ROSTER_SIZE);
            }
            other => panic!("expected RosterValidated, got {other:?}"),
        }
        assert_eq!(boss.version(), 1);
        assert_eq!(boss.uncommitted_events().len(), 1);
        assert_eq!(
            boss.uncommitted_events()[0].event_type(),
            "boss.roster.validated"
        );
    }

    // Scenario: rejected — every Boss has exactly one hero power and exactly one
    // trademark.
    #[test]
    fn validate_rejects_when_signature_shape_is_violated() {
        let mut boss = valid_boss();
        // A second trademark breaks the exactly-one shape.
        boss.declare_trademark("The Getaway Car");

        let err = boss
            .execute(valid_roster_cmd().into_command())
            .expect_err("a Boss with two trademarks must fail validation");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(boss.version(), 0);
    }

    // Scenario: rejected — every Boss must be assigned a non-empty signature card
    // set drawn from valid CardDefinitions.
    #[test]
    fn validate_rejects_when_signature_set_is_empty() {
        let mut boss = valid_boss();
        // Clear the standing signature set the roster validation depends on.
        boss.assign_signature_set(Vec::<String>::new());

        let err = boss
            .execute(valid_roster_cmd().into_command())
            .expect_err("a Boss without a signature set must fail validation");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(boss.version(), 0);
    }

    // Scenario: rejected — Boss starting HP must fall within the legal HP bounds.
    #[test]
    fn validate_rejects_when_starting_hp_is_out_of_bounds() {
        let mut boss = valid_boss();
        // One below the legal minimum.
        boss.set_starting_hp(LEGAL_STARTING_HP.start() - 1);

        let err = boss
            .execute(valid_roster_cmd().into_command())
            .expect_err("HP outside the legal bounds must fail validation");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(boss.version(), 0);
    }

    // Scenario: rejected — the published roster must contain exactly the
    // authoritative 18 Bosses before a launch is validated.
    #[test]
    fn validate_rejects_when_published_roster_is_not_authoritative_size() {
        let mut boss = valid_boss();
        // One over the authoritative roster.
        boss.set_published_roster_size(AUTHORITATIVE_ROSTER_SIZE + 1);

        let err = boss
            .execute(valid_roster_cmd().into_command())
            .expect_err("a roster of the wrong size must fail validation");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(boss.version(), 0);
    }

    // A command whose expectedCount disagrees with the authoritative roster size
    // is rejected before any state is inspected.
    #[test]
    fn validate_rejects_when_expected_count_is_not_authoritative() {
        let mut boss = valid_boss();
        let cmd = ValidateBossRoster::new("roster-2026", AUTHORITATIVE_ROSTER_SIZE - 1);

        let err = boss
            .execute(cmd.into_command())
            .expect_err("a non-authoritative expected count must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(boss.version(), 0);
    }

    #[test]
    fn validate_command_payload_round_trips() {
        let cmd = valid_roster_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, ValidateBossRoster::COMMAND);
        let decoded: ValidateBossRoster = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_roster_cmd());
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut boss = BossDefinition::new("boss-01");
        let err = boss.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "BossDefinition");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, AssignSignatureCards::COMMAND);
        let decoded: AssignSignatureCards = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
