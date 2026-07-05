//! ExpansionSet bounded context — released card sets/expansions and their cards.
//!
//! An [`ExpansionSet`] is the catalog entry for one draft expansion: its set
//! code, the cards it ships, the keywords it introduces, and the declared
//! card-count bounds an expansion must satisfy. Before a set can be released it
//! must be *validated*, and three invariants keep a draft releasable:
//!
//! 1. **Validator** — every draft card must pass the validator: it must be
//!    schema-well-formed (a non-empty id and class), its cost and class must be
//!    legal ([`LEGAL_CARD_COST`] / the registered legal classes), and it must
//!    respect the copy cap ([`MAX_COPIES_PER_CARD`]). An expansion cannot be
//!    released until it passes the validator.
//! 2. **Keyword registration** — any new keyword introduced by the set must be
//!    registered and implemented in the engine before release; an introduced
//!    keyword absent from the engine's registered set blocks validation.
//! 3. **Card count** — the set's card count must fall within the declared
//!    [`EXPANSION_CARD_COUNT_BOUNDS`] for an expansion.
//!
//! One command is implemented. [`ValidateExpansion`] (`ValidateExpansionCmd`)
//! runs the validator over the draft set, enforcing all three invariants and, on
//! success, emitting [`Event::ExpansionValidated`] (`expansion.validated`). This
//! module is hand-written (it no longer uses `shared::stub_aggregate!`) but
//! preserves the same public surface — an [`ExpansionSet`] aggregate and an
//! [`ExpansionSetRepository`] port — so the persistence adapters in
//! `crates/mocks` keep compiling unchanged.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "ExpansionSet";

/// The command name [`ExpansionSet::execute`] recognizes for validating a set.
const VALIDATE_EXPANSION: &str = "ValidateExpansionCmd";

/// The inclusive `[min, max]` legal range for a draft card's mana cost. A card
/// whose cost falls outside these authored bounds fails the validator's
/// cost-legality check, mirroring the cost ranges the
/// [`CardDefinition`](crate::card_definition) context carries.
pub const LEGAL_CARD_COST: std::ops::RangeInclusive<i64> = 0..=12;

/// The copy cap: no single card may ship more than this many copies in a set.
pub const MAX_COPIES_PER_CARD: u32 = 2;

/// The inclusive `[min, max]` declared bounds for the number of cards in an
/// expansion. A draft whose card count falls outside this range cannot be
/// released.
pub const EXPANSION_CARD_COUNT_BOUNDS: std::ops::RangeInclusive<usize> = 100..=250;

/// A single draft card in an expansion, in its raw as-submitted form. The
/// validator checks each of these against schema, cost/class legality, and the
/// copy cap. Field names are the catalog's `camelCase` schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftCard {
    /// The card's identity within the set; must be non-empty (schema).
    pub card_id: String,
    /// The card's class; must be non-empty (schema) and one of the registered
    /// legal classes (class legality).
    pub class: String,
    /// The card's mana cost; must fall within [`LEGAL_CARD_COST`].
    pub cost: i64,
    /// How many copies of the card ship in the set; must be within the copy cap
    /// `1..=`[`MAX_COPIES_PER_CARD`].
    pub copies: u32,
}

/// The `ValidateExpansionCmd` payload: the set code to validate. Field names are
/// the catalog's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ValidateExpansion::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`ExpansionSet::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateExpansion {
    /// The set code being validated; must name the set this aggregate records.
    pub set_code: String,
}

impl ValidateExpansion {
    /// The command name this maps to.
    pub const COMMAND: &'static str = VALIDATE_EXPANSION;

    /// Build a command validating the set `set_code`.
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

/// A validated expansion, produced once every invariant has been checked.
/// Carried by [`Event::ExpansionValidated`] and thus by the emitted
/// `expansion.validated` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpansionValidated {
    /// The set that was validated.
    pub set_code: String,
    /// The proven-legal card count of the set.
    pub card_count: usize,
}

/// Domain events emitted by [`ExpansionSet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A draft set passed every invariant and is cleared for release.
    ExpansionValidated(ExpansionValidated),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::ExpansionValidated(_) => "expansion.validated",
        }
    }
}

/// The ExpansionSet aggregate: the catalog entry for a single draft expansion.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the server — is unchanged, while it now carries the
/// draft's cards, the keywords it introduces, the engine's registered keywords,
/// and the legal classes, and its `execute` handles [`ValidateExpansionCmd`].
///
/// A fresh set from [`ExpansionSet::new`] is intentionally *empty* (no cards, no
/// keywords, no legal classes); the configuration methods below build it up to a
/// releasable shape before a command validates it, exactly as
/// [`BossDefinition`](crate::boss_definition) is configured before a command
/// validates it.
#[derive(Debug)]
pub struct ExpansionSet {
    id: String,
    root: AggregateRoot,
    /// The set code this aggregate records. A `ValidateExpansionCmd` must name
    /// this same set.
    set_code: String,
    /// The draft cards the set ships; validated against schema, cost/class
    /// legality, and the copy cap.
    draft_cards: Vec<DraftCard>,
    /// The keywords this set introduces; each must be registered in the engine.
    introduced_keywords: Vec<String>,
    /// The keywords registered and implemented in the engine.
    registered_keywords: BTreeSet<String>,
    /// The legal classes a draft card's class must be drawn from.
    legal_classes: BTreeSet<String>,
}

impl ExpansionSet {
    /// Create a new, empty expansion identified by `id`. Use the configuration
    /// methods to bring it to a releasable shape before validating it.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            set_code: id.clone(),
            id,
            root: AggregateRoot::new(),
            draft_cards: Vec::new(),
            introduced_keywords: Vec::new(),
            registered_keywords: BTreeSet::new(),
            legal_classes: BTreeSet::new(),
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The set this aggregate records.
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

    /// Add a draft card to the set.
    pub fn add_draft_card(&mut self, card: DraftCard) {
        self.draft_cards.push(card);
    }

    /// Declare a keyword the set introduces. Each introduced keyword must be
    /// registered in the engine for validation to pass.
    pub fn introduce_keyword(&mut self, keyword: impl Into<String>) {
        self.introduced_keywords.push(keyword.into());
    }

    /// Register the keywords implemented in the engine that introduced keywords
    /// may resolve to.
    pub fn register_engine_keywords<I, S>(&mut self, keywords: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.registered_keywords = keywords.into_iter().map(Into::into).collect();
    }

    /// Register the legal classes a draft card's class must be drawn from.
    pub fn register_legal_classes<I, S>(&mut self, classes: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.legal_classes = classes.into_iter().map(Into::into).collect();
    }

    /// Validator invariant: every draft card is schema-well-formed and passes
    /// cost/class legality and the copy cap. An expansion cannot be released
    /// until it passes the validator.
    fn ensure_passes_validator(&self) -> Result<(), DomainError> {
        for card in &self.draft_cards {
            // Schema: a card must have a non-empty id and class.
            if card.card_id.trim().is_empty() {
                return Err(DomainError::InvariantViolation(
                    "draft card fails schema: empty card id".to_string(),
                ));
            }
            if card.class.trim().is_empty() {
                return Err(DomainError::InvariantViolation(format!(
                    "draft card '{}' fails schema: empty class",
                    card.card_id
                )));
            }
            // Cost legality.
            if !LEGAL_CARD_COST.contains(&card.cost) {
                return Err(DomainError::InvariantViolation(format!(
                    "draft card '{}' cost {} is outside the legal range [{}, {}]",
                    card.card_id,
                    card.cost,
                    LEGAL_CARD_COST.start(),
                    LEGAL_CARD_COST.end()
                )));
            }
            // Class legality.
            if !self.legal_classes.contains(&card.class) {
                return Err(DomainError::InvariantViolation(format!(
                    "draft card '{}' class '{}' is not a legal class",
                    card.card_id, card.class
                )));
            }
            // Copy cap.
            if card.copies == 0 || card.copies > MAX_COPIES_PER_CARD {
                return Err(DomainError::InvariantViolation(format!(
                    "draft card '{}' ships {} copies, outside the copy cap [1, {}]",
                    card.card_id, card.copies, MAX_COPIES_PER_CARD
                )));
            }
        }
        Ok(())
    }

    /// Keyword-registration invariant: every keyword the set introduces is
    /// registered and implemented in the engine.
    fn ensure_keywords_registered(&self) -> Result<(), DomainError> {
        for keyword in &self.introduced_keywords {
            if !self.registered_keywords.contains(keyword) {
                return Err(DomainError::InvariantViolation(format!(
                    "keyword '{keyword}' introduced by the set is not registered and implemented \
                     in the engine"
                )));
            }
        }
        Ok(())
    }

    /// Card-count invariant: the set's card count falls within the declared
    /// [`EXPANSION_CARD_COUNT_BOUNDS`].
    fn ensure_card_count(&self) -> Result<(), DomainError> {
        let count = self.draft_cards.len();
        if !EXPANSION_CARD_COUNT_BOUNDS.contains(&count) {
            return Err(DomainError::InvariantViolation(format!(
                "set card count must fall within the declared bounds [{}, {}]; set '{}' has {}",
                EXPANSION_CARD_COUNT_BOUNDS.start(),
                EXPANSION_CARD_COUNT_BOUNDS.end(),
                self.set_code,
                count
            )));
        }
        Ok(())
    }

    /// Handle `ValidateExpansionCmd`: verify the command targets this set, run
    /// the validator over the draft set (all three invariants), and emit
    /// [`Event::ExpansionValidated`].
    ///
    /// Every invariant is checked before any event is recorded, so a rejected
    /// command leaves the aggregate untouched.
    fn validate_expansion(&mut self, cmd: ValidateExpansion) -> Result<Vec<Event>, DomainError> {
        // The command must name the set this aggregate actually records.
        if cmd.set_code != self.set_code {
            return Err(DomainError::InvariantViolation(format!(
                "command targets set '{}' but this aggregate records '{}'",
                cmd.set_code, self.set_code
            )));
        }

        // Run the validator over the draft set.
        self.ensure_passes_validator()?;
        self.ensure_keywords_registered()?;
        self.ensure_card_count()?;

        let event = Event::ExpansionValidated(ExpansionValidated {
            set_code: cmd.set_code,
            card_count: self.draft_cards.len(),
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

    /// The smallest legal card count for an expansion; the valid fixtures ship
    /// exactly this many cards.
    const VALID_CARD_COUNT: usize = *EXPANSION_CARD_COUNT_BOUNDS.start();

    /// Build `count` schema-well-formed, legal, copy-cap-respecting draft cards.
    fn valid_cards(count: usize) -> Vec<DraftCard> {
        (0..count)
            .map(|i| DraftCard {
                card_id: format!("card-{i:04}"),
                class: "Neutral".to_string(),
                // Cycle costs across the legal range so cost legality is exercised.
                cost: (i as i64) % (LEGAL_CARD_COST.end() + 1),
                copies: 1,
            })
            .collect()
    }

    /// A releasable draft set `SET-01`: a legal-sized batch of valid cards, one
    /// introduced keyword that the engine has registered, and `Neutral` as a
    /// legal class. Tests mutate one aspect at a time to drive a specific
    /// rejection.
    fn valid_set() -> ExpansionSet {
        let mut set = ExpansionSet::new("SET-01");
        for card in valid_cards(VALID_CARD_COUNT) {
            set.add_draft_card(card);
        }
        set.register_legal_classes(["Neutral"]);
        set.introduce_keyword("Overload");
        set.register_engine_keywords(["Overload", "Rush"]);
        set
    }

    /// A `ValidateExpansionCmd` targeting the same `SET-01` as [`valid_set`].
    fn valid_cmd() -> ValidateExpansion {
        ValidateExpansion::new("SET-01")
    }

    // Scenario: Successfully execute ValidateExpansionCmd.
    #[test]
    fn validates_expansion_and_emits_expansion_validated_event() {
        let mut set = valid_set();

        let events = set
            .execute(valid_cmd().into_command())
            .expect("a valid draft set should validate");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "expansion.validated");
        match &events[0] {
            Event::ExpansionValidated(validated) => {
                assert_eq!(validated.set_code, "SET-01");
                assert_eq!(validated.card_count, VALID_CARD_COUNT);
            }
        }
        // The event was recorded on the aggregate root.
        assert_eq!(set.version(), 1);
        assert_eq!(set.uncommitted_events().len(), 1);
        assert_eq!(
            set.uncommitted_events()[0].event_type(),
            "expansion.validated"
        );
    }

    // Scenario: rejected — an expansion cannot be released until it passes the
    // validator (schema, cost/class legality, copy caps). Here a card's cost is
    // outside the legal range.
    #[test]
    fn rejects_when_a_card_fails_the_validator_cost() {
        let mut set = valid_set();
        set.add_draft_card(DraftCard {
            card_id: "card-illegal".to_string(),
            class: "Neutral".to_string(),
            cost: LEGAL_CARD_COST.end() + 1,
            copies: 1,
        });

        let err = set
            .execute(valid_cmd().into_command())
            .expect_err("a card that fails the validator must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: rejected — the validator also enforces the copy cap.
    #[test]
    fn rejects_when_a_card_exceeds_the_copy_cap() {
        let mut set = valid_set();
        set.add_draft_card(DraftCard {
            card_id: "card-overcopied".to_string(),
            class: "Neutral".to_string(),
            cost: 3,
            copies: MAX_COPIES_PER_CARD + 1,
        });

        let err = set
            .execute(valid_cmd().into_command())
            .expect_err("a card over the copy cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: rejected — the validator also enforces class legality.
    #[test]
    fn rejects_when_a_card_has_an_illegal_class() {
        let mut set = valid_set();
        set.add_draft_card(DraftCard {
            card_id: "card-badclass".to_string(),
            class: "Nonexistent".to_string(),
            cost: 3,
            copies: 1,
        });

        let err = set
            .execute(valid_cmd().into_command())
            .expect_err("a card with an illegal class must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: rejected — any new keyword introduced by the set must be
    // registered and implemented in the engine before release.
    #[test]
    fn rejects_when_an_introduced_keyword_is_not_registered() {
        let mut set = valid_set();
        // A brand-new keyword the engine has not implemented.
        set.introduce_keyword("Photosynthesize");

        let err = set
            .execute(valid_cmd().into_command())
            .expect_err("an unregistered introduced keyword must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: rejected — set card count must fall within the declared bounds
    // for an expansion. Here the set is one card short of the minimum.
    #[test]
    fn rejects_when_card_count_below_declared_bounds() {
        let mut set = ExpansionSet::new("SET-01");
        for card in valid_cards(*EXPANSION_CARD_COUNT_BOUNDS.start() - 1) {
            set.add_draft_card(card);
        }
        set.register_legal_classes(["Neutral"]);

        let err = set
            .execute(valid_cmd().into_command())
            .expect_err("a card count below the declared bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // Scenario: rejected — set card count must fall within the declared bounds
    // for an expansion. Here the set is one card over the maximum.
    #[test]
    fn rejects_when_card_count_above_declared_bounds() {
        let mut set = ExpansionSet::new("SET-01");
        for card in valid_cards(*EXPANSION_CARD_COUNT_BOUNDS.end() + 1) {
            set.add_draft_card(card);
        }
        set.register_legal_classes(["Neutral"]);

        let err = set
            .execute(valid_cmd().into_command())
            .expect_err("a card count above the declared bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
    }

    // A command targeting a different set than this aggregate records is rejected.
    #[test]
    fn rejects_command_targeting_a_different_set() {
        let mut set = valid_set();
        let err = set
            .execute(ValidateExpansion::new("SET-99").into_command())
            .expect_err("a command for another set must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(set.version(), 0);
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
        assert_eq!(command.name, ValidateExpansion::COMMAND);
        let decoded: ValidateExpansion = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
