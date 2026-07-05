//! ExpansionSet bounded context — released card sets/expansions and their cards.
//!
//! This context grows real behavior beyond the [`shared::stub_aggregate!`]
//! scaffold. It handles a single write command, [`DraftExpansionCmd`], which
//! assembles candidate cards and keywords into a *draft* set and, when every
//! release invariant holds, emits an [`Event::ExpansionDrafted`]
//! (`expansion.drafted`) event.
//!
//! The aggregate follows the kernel's `execute(cmd)` port: [`ExpansionSet`]
//! decodes the command's opaque JSON payload into a typed [`DraftExpansionCmd`],
//! parses each raw field into a value object (so illegal states become
//! unrepresentable past the boundary), enforces the expansion invariants, and
//! records the resulting event on its [`shared::AggregateRoot`].
//!
//! Enforced invariants, drawn from the release rules of an expansion:
//!
//! 1. *An expansion cannot be released until it passes the validator (schema,
//!    cost/class legality, copy caps).*
//! 2. *Any new keyword introduced by the set must be registered and implemented
//!    in the engine before release.*
//! 3. *Set card count must fall within the declared bounds for an expansion.*

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "ExpansionSet";

/// The command name [`ExpansionSet::execute`] recognizes.
const DRAFT_EXPANSION: &str = "DraftExpansionCmd";

/// Inclusive lower bound on the number of cards a released expansion may hold.
/// Below this an expansion is too thin to constitute a set.
pub const MIN_SET_CARDS: usize = 50;

/// Inclusive upper bound on the number of cards a released expansion may hold.
/// Above this an expansion exceeds the declared bounds for a set.
pub const MAX_SET_CARDS: usize = 250;

/// Per-set copy cap: within a single expansion draft a card may appear at most
/// once. A card listed twice violates the validator's copy-cap check.
const MAX_COPIES_PER_SET: usize = 1;

/// Keywords the game engine has registered and implemented. A keyword the draft
/// introduces via `newKeywords` must resolve to one of these before the set can
/// be released (invariant 2). In a fuller system this table would be sourced
/// from the engine's keyword registry; modeling it as domain-visible knowledge
/// keeps the aggregate self-contained and testable.
pub const REGISTERED_KEYWORDS: &[&str] = &[
    "Stealth",
    "Overwatch",
    "Heat",
    "Launder",
    "Getaway",
    "Muscle",
    "Wildcard",
    "Doublecross",
];

/// A validated set code: the schema-legal identity an expansion is cataloged
/// under. Well-formed codes are 2–8 uppercase alphanumeric characters (e.g.
/// `"HEIST01"`). Enforcing the shape here is the *schema* half of the validator
/// gate (invariant 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetCode(String);

impl SetCode {
    /// Parse a raw set code, rejecting anything that fails the schema.
    fn parse(raw: &str) -> Result<Self, DomainError> {
        let trimmed = raw.trim();
        let well_formed = (2..=8).contains(&trimmed.len())
            && trimmed
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        if !well_formed {
            return Err(DomainError::InvariantViolation(format!(
                "set code '{raw}' fails the validator schema: expected 2-8 uppercase alphanumeric characters"
            )));
        }
        Ok(SetCode(trimmed.to_string()))
    }

    /// The validated code as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The `DraftExpansionCmd` payload: a proposed expansion draft in its raw,
/// as-submitted form. Field names are the catalog's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`DraftExpansionCmd::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`ExpansionSet::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftExpansionCmd {
    /// Schema-legal identity the expansion is cataloged under.
    pub set_code: String,
    /// The card definitions assembled into the draft set.
    pub card_ids: Vec<String>,
    /// Keywords the set newly introduces; each must be engine-registered.
    #[serde(default)]
    pub new_keywords: Vec<String>,
}

impl DraftExpansionCmd {
    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`ExpansionSet::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("DraftExpansionCmd is always serializable");
        Command::with_payload(DRAFT_EXPANSION, payload)
    }
}

/// A validated expansion draft, produced once every invariant has been checked.
/// Carried by [`Event::ExpansionDrafted`] and thus by the emitted
/// `expansion.drafted` event; every field is a proven-legal value object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpansionDrafted {
    pub set_code: SetCode,
    pub card_ids: Vec<String>,
    pub new_keywords: Vec<String>,
}

/// Domain events emitted by [`ExpansionSet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A draft expansion passed validation and was assembled into a set.
    ExpansionDrafted(ExpansionDrafted),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::ExpansionDrafted(_) => "expansion.drafted",
        }
    }
}

/// The ExpansionSet aggregate: a released card set and the cards it collects.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the server — is unchanged, while `execute` now handles
/// [`DraftExpansionCmd`].
#[derive(Debug)]
pub struct ExpansionSet {
    id: String,
    root: AggregateRoot,
}

impl ExpansionSet {
    /// Create a new aggregate instance with the given identity.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
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

    /// Validate a proposed expansion draft and, if it holds, record and return
    /// the resulting `expansion.drafted` event.
    ///
    /// Each invariant is checked in turn; the first failure short-circuits with
    /// a [`DomainError::InvariantViolation`], leaving the aggregate untouched.
    fn draft_expansion(&mut self, cmd: DraftExpansionCmd) -> Result<Vec<Event>, DomainError> {
        // Invariant 1 (validator — schema): the set code must be schema-legal.
        let set_code = SetCode::parse(&cmd.set_code)?;

        // Invariant 1 (validator — copy caps): no card may exceed its per-set
        // copy cap. A card listed more than `MAX_COPIES_PER_SET` times fails the
        // validator before the set can be released.
        let mut seen: HashSet<&str> = HashSet::with_capacity(cmd.card_ids.len());
        for card_id in &cmd.card_ids {
            let id = card_id.trim();
            if id.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "expansion draft contains an empty card id, which fails the validator schema"
                        .to_string(),
                ));
            }
            if !seen.insert(id) {
                return Err(DomainError::InvariantViolation(format!(
                    "card '{id}' appears more than the per-set copy cap of {MAX_COPIES_PER_SET}; the validator rejects the expansion"
                )));
            }
        }

        // Invariant 3: set card count must fall within the declared bounds.
        let count = cmd.card_ids.len();
        if !(MIN_SET_CARDS..=MAX_SET_CARDS).contains(&count) {
            return Err(DomainError::InvariantViolation(format!(
                "set card count {count} falls outside the declared bounds [{MIN_SET_CARDS}, {MAX_SET_CARDS}] for an expansion"
            )));
        }

        // Invariant 2: every newly-introduced keyword must be engine-registered.
        for keyword in &cmd.new_keywords {
            if !REGISTERED_KEYWORDS.contains(&keyword.as_str()) {
                return Err(DomainError::InvariantViolation(format!(
                    "keyword '{keyword}' is not registered and implemented in the engine; it must be before release"
                )));
            }
        }

        let event = Event::ExpansionDrafted(ExpansionDrafted {
            set_code,
            card_ids: cmd.card_ids,
            new_keywords: cmd.new_keywords,
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
            DRAFT_EXPANSION => {
                let cmd: DraftExpansionCmd =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed DraftExpansionCmd payload: {e}"
                        ))
                    })?;
                self.draft_expansion(cmd)
            }
            _ => Err(DomainError::unknown_command(AGGREGATE_TYPE, command.name)),
        }
    }
}

/// Repository contract for the [`ExpansionSet`] aggregate. Adapters implement
/// [`shared::Repository`] for `ExpansionSet` and then this marker trait.
pub trait ExpansionSetRepository: Repository<ExpansionSet> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `DraftExpansionCmd` that satisfies every invariant, as a starting
    /// point tests mutate one field at a time to drive a specific rejection.
    fn valid_cmd() -> DraftExpansionCmd {
        DraftExpansionCmd {
            set_code: "HEIST01".to_string(),
            card_ids: (0..MIN_SET_CARDS).map(|i| format!("card-{i:03}")).collect(),
            new_keywords: vec!["Stealth".to_string(), "Heat".to_string()],
        }
    }

    #[test]
    fn draft_expansion_emits_expansion_drafted_event() {
        let mut agg = ExpansionSet::new("set-heist01");
        let events = agg.execute(valid_cmd().into_command()).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "expansion.drafted");
        assert_eq!(agg.version(), 1);
        assert_eq!(agg.uncommitted_events().len(), 1);
        assert_eq!(
            agg.uncommitted_events()[0].event_type(),
            "expansion.drafted"
        );
    }

    #[test]
    fn draft_with_no_new_keywords_is_accepted() {
        let mut agg = ExpansionSet::new("set-heist01");
        let cmd = DraftExpansionCmd {
            new_keywords: vec![],
            ..valid_cmd()
        };
        assert!(agg.execute(cmd.into_command()).is_ok());
    }

    #[test]
    fn rejects_set_code_that_fails_the_validator_schema() {
        let mut agg = ExpansionSet::new("set-bad");
        // Lowercase + punctuation is not a schema-legal set code.
        let cmd = DraftExpansionCmd {
            set_code: "heist-1!".to_string(),
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    #[test]
    fn rejects_duplicate_card_that_exceeds_the_copy_cap() {
        let mut agg = ExpansionSet::new("set-heist01");
        let mut card_ids: Vec<String> =
            (0..MIN_SET_CARDS).map(|i| format!("card-{i:03}")).collect();
        // Duplicate one id so the validator's copy-cap check trips.
        card_ids.push("card-000".to_string());
        let cmd = DraftExpansionCmd {
            card_ids,
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    #[test]
    fn rejects_unregistered_new_keyword() {
        let mut agg = ExpansionSet::new("set-heist01");
        let cmd = DraftExpansionCmd {
            new_keywords: vec!["Teleport".to_string()],
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    #[test]
    fn rejects_card_count_below_declared_bounds() {
        let mut agg = ExpansionSet::new("set-heist01");
        let cmd = DraftExpansionCmd {
            card_ids: (0..MIN_SET_CARDS - 1)
                .map(|i| format!("card-{i:03}"))
                .collect(),
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    #[test]
    fn rejects_card_count_above_declared_bounds() {
        let mut agg = ExpansionSet::new("set-heist01");
        let cmd = DraftExpansionCmd {
            card_ids: (0..MAX_SET_CARDS + 1)
                .map(|i| format!("card-{i:04}"))
                .collect(),
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    #[test]
    fn rejects_unknown_command() {
        let mut agg = ExpansionSet::new("set-heist01");
        let err = agg.execute(Command::new("NoSuchCommand")).unwrap_err();
        assert!(matches!(
            err,
            DomainError::UnknownCommand {
                aggregate: "ExpansionSet",
                ..
            }
        ));
    }
}
