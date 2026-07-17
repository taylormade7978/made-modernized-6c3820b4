//! CardDefinition bounded context — the catalog of playable card definitions.
//!
//! This is the first bounded context to grow real behavior beyond the
//! [`shared::stub_aggregate!`] scaffold. It handles two write commands:
//! [`DefineCardCmd`], which catalogs a new card, and [`ReviseCardCmd`], which
//! amends an existing card's definition (balance/text). Both validate the
//! proposed fields against the catalog schema and, when every invariant holds,
//! emit a [`Event::CardDefined`] (`card.defined`) or [`Event::CardRevised`]
//! (`card.revised`) event respectively.
//!
//! The aggregate follows the kernel's `execute(cmd)` port: [`CardDefinition`]
//! decodes the command's opaque JSON payload into a typed command, parses each
//! raw field into a value object (so illegal states become unrepresentable past
//! the boundary), enforces the catalog invariants via the shared
//! [`validate_card_fields`] check, and records the resulting event on its
//! [`shared::AggregateRoot`]. Definition and revision run the *same* invariants,
//! so an amendment can never move a card into an illegal state.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "CardDefinition";

/// The command name [`CardDefinition::execute`] recognizes to catalog a new card.
const DEFINE_CARD: &str = "DefineCardCmd";

/// The command name [`CardDefinition::execute`] recognizes to amend an existing
/// card definition (balance/text) and re-validate it.
const REVISE_CARD: &str = "ReviseCardCmd";

/// Effect-script references the game engine has registered. A card's
/// `effectScriptRef` must resolve to one of these for the definition to be
/// accepted (see [invariant 4][`define_card_invariants`]). In a fuller system
/// this table would be sourced from the engine's effect registry; modeling it
/// as domain-visible knowledge keeps the aggregate self-contained and testable.
pub const REGISTERED_EFFECTS: &[&str] = &[
    "effect.noop",
    "effect.deal_damage",
    "effect.draw_card",
    "effect.gain_juice",
    "effect.cool",
    "effect.steal_piece",
    "effect.recruit_operator",
    "effect.pull_heist",
];

/// The five card types every card is exactly one of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CardType {
    Operator,
    Job,
    Piece,
    Vehicle,
    Heist,
}

impl CardType {
    /// Parse a raw type name, rejecting anything outside the five legal types.
    ///
    /// Enforces: *Every card is exactly one of the five card types: Operator,
    /// Job, Piece, Vehicle, or Heist.*
    fn parse(raw: &str) -> Result<Self, DomainError> {
        match raw {
            "Operator" => Ok(CardType::Operator),
            "Job" => Ok(CardType::Job),
            "Piece" => Ok(CardType::Piece),
            "Vehicle" => Ok(CardType::Vehicle),
            "Heist" => Ok(CardType::Heist),
            other => Err(DomainError::InvariantViolation(format!(
                "card type must be exactly one of Operator, Job, Piece, Vehicle, or Heist; got '{other}'"
            ))),
        }
    }

    /// The inclusive `[min, max]` Juice cost range legal for this card type.
    fn legal_cost_range(self) -> (i64, i64) {
        match self {
            CardType::Operator => (1, 8),
            CardType::Job => (0, 6),
            CardType::Piece => (0, 5),
            CardType::Vehicle => (2, 9),
            CardType::Heist => (3, 12),
        }
    }

    /// Canonical name, used in the recorded event.
    fn as_str(self) -> &'static str {
        match self {
            CardType::Operator => "Operator",
            CardType::Job => "Job",
            CardType::Piece => "Piece",
            CardType::Vehicle => "Vehicle",
            CardType::Heist => "Heist",
        }
    }
}

/// A card's class allegiance. A card belongs to exactly one class, or is
/// [`CardClass::Neutral`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CardClass {
    Neutral,
    Boss,
    Muscle,
    Grifter,
    Hacker,
    Driver,
    Cleaner,
}

impl CardClass {
    /// Parse the raw class field into exactly one class.
    ///
    /// Enforces: *A card belongs to exactly one class or is Neutral; no card
    /// may claim two classes.* The raw value is tokenized on the separators a
    /// caller might use to name multiple classes (`/ , + &`); more than one
    /// token is rejected outright, and the single token must name a known class.
    fn parse(raw: &str) -> Result<Self, DomainError> {
        let tokens: Vec<&str> = raw
            .split(['/', ',', '+', '&'])
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();

        match tokens.as_slice() {
            [] => Err(DomainError::InvariantViolation(
                "a card must declare exactly one class or Neutral; none was provided".to_string(),
            )),
            [single] => Self::parse_single(single),
            _ => Err(DomainError::InvariantViolation(format!(
                "a card belongs to exactly one class or is Neutral; no card may claim two classes; got '{raw}'"
            ))),
        }
    }

    fn parse_single(raw: &str) -> Result<Self, DomainError> {
        match raw {
            "Neutral" => Ok(CardClass::Neutral),
            "Boss" => Ok(CardClass::Boss),
            "Muscle" => Ok(CardClass::Muscle),
            "Grifter" => Ok(CardClass::Grifter),
            "Hacker" => Ok(CardClass::Hacker),
            "Driver" => Ok(CardClass::Driver),
            "Cleaner" => Ok(CardClass::Cleaner),
            other => Err(DomainError::InvariantViolation(format!(
                "unknown card class '{other}'; a card must name one known class or Neutral"
            ))),
        }
    }
}

/// Card rarity. [`Rarity::Legendary`] carries a per-Outfit copy cap of 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Epic,
    Legendary,
}

impl Rarity {
    fn parse(raw: &str) -> Result<Self, DomainError> {
        match raw {
            "Common" => Ok(Rarity::Common),
            "Uncommon" => Ok(Rarity::Uncommon),
            "Rare" => Ok(Rarity::Rare),
            "Epic" => Ok(Rarity::Epic),
            "Legendary" => Ok(Rarity::Legendary),
            other => Err(DomainError::InvariantViolation(format!(
                "unknown rarity '{other}'"
            ))),
        }
    }
}

/// The `DefineCardCmd` payload: a proposed card definition in its raw,
/// as-submitted form. Field names are the catalog's `camelCase` schema; `type`
/// is spelled out explicitly since it is a Rust keyword.
///
/// Build one directly and turn it into a [`Command`] with
/// [`DefineCardCmd::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`CardDefinition::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefineCardCmd {
    /// Identity the card is cataloged under.
    pub card_id: String,
    /// Human-readable card name.
    pub name: String,
    /// Juice cost. Must fall within the legal range for the card's type.
    pub cost: i64,
    /// Class allegiance (one class, or `Neutral`).
    pub class: String,
    /// One of the five card types.
    #[serde(rename = "type")]
    pub card_type: String,
    /// Card rarity.
    pub rarity: String,
    /// Keyword tags on the card.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Reference to the effect script the card runs; must be registered.
    pub effect_script_ref: String,
    /// Per-Outfit copy cap declared on the definition. Required to be `1` for
    /// [`Rarity::Legendary`]; defaults to `0` when omitted.
    #[serde(default)]
    pub copy_cap: u32,
    /// Board attack power. 0 for non-unit (spell-like) types.
    #[serde(default)]
    pub atk: u8,
    /// Board health. Must be >= 1 for Operator/Vehicle; 0 for Job/Piece/Heist.
    #[serde(default)]
    pub hp: u8,
    /// If `Some(boss_id)`, only that Boss's Outfit may deck this card. A
    /// boss-locked card must be of class [`CardClass::Boss`] (Task 8).
    #[serde(default)]
    pub boss_lock: Option<String>,
}

impl DefineCardCmd {
    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`CardDefinition::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("DefineCardCmd is always serializable");
        Command::with_payload(DEFINE_CARD, payload)
    }
}

/// The `ReviseCardCmd` payload: the complete amended definition for a card that
/// already exists in the catalog. A revision (balance or text changes) submits
/// the full revised card rather than a partial patch — the aggregate keeps no
/// prior field state to merge against — and the whole definition is re-validated
/// against the catalog schema before a `card.revised` event is emitted.
///
/// Field shape mirrors [`DefineCardCmd`] so the same value objects and the same
/// [`validate_card_fields`] invariants apply to an amendment as to a definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviseCardCmd {
    /// Identity of the existing card being revised.
    pub card_id: String,
    /// Revised human-readable card name.
    pub name: String,
    /// Revised Juice cost. Must fall within the legal range for the card's type.
    pub cost: i64,
    /// Revised class allegiance (one class, or `Neutral`).
    pub class: String,
    /// Revised card type; one of the five legal types.
    #[serde(rename = "type")]
    pub card_type: String,
    /// Revised rarity.
    pub rarity: String,
    /// Revised keyword tags on the card.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Revised effect-script reference; must resolve to a registered effect.
    pub effect_script_ref: String,
    /// Per-Outfit copy cap declared on the revised definition. Required to be
    /// `1` for [`Rarity::Legendary`]; defaults to `0` when omitted.
    #[serde(default)]
    pub copy_cap: u32,
    /// Revised board attack power. 0 for non-unit (spell-like) types.
    #[serde(default)]
    pub atk: u8,
    /// Revised board health. Must be >= 1 for Operator/Vehicle; 0 for Job/Piece/Heist.
    #[serde(default)]
    pub hp: u8,
    /// If `Some(boss_id)`, only that Boss's Outfit may deck this card. A
    /// boss-locked card must be of class [`CardClass::Boss`] (Task 8).
    #[serde(default)]
    pub boss_lock: Option<String>,
}

impl ReviseCardCmd {
    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`CardDefinition::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ReviseCardCmd is always serializable");
        Command::with_payload(REVISE_CARD, payload)
    }
}

/// The proven-legal value objects produced by [`validate_card_fields`]: the raw
/// `type`, `class`, and `rarity` strings parsed into their enums once every
/// catalog invariant holds. Fields carried straight through to the event (name,
/// cost, keywords, …) are not re-wrapped here; only the parsed ones are.
struct ValidatedCardFields {
    class: CardClass,
    card_type: CardType,
    rarity: Rarity,
}

/// Enforce every catalog invariant on a card's raw fields, returning the parsed
/// value objects when they all hold, or the first [`DomainError::InvariantViolation`].
///
/// This is the single source of truth for card validity: both
/// [`CardDefinition::define_card`] (cataloging a new card) and
/// [`CardDefinition::revise_card`] (amending an existing one) run it, so a
/// revision is held to exactly the same schema as the original definition.
#[allow(clippy::too_many_arguments)]
fn validate_card_fields(
    name: &str,
    raw_card_type: &str,
    raw_class: &str,
    raw_rarity: &str,
    cost: i64,
    effect_script_ref: &str,
    copy_cap: u32,
    atk: u8,
    hp: u8,
    boss_lock: &Option<String>,
) -> Result<ValidatedCardFields, DomainError> {
    if name.trim().is_empty() {
        return Err(DomainError::InvariantViolation(
            "a card must have a non-empty name".to_string(),
        ));
    }

    // Invariant: exactly one card type (or reject).
    let card_type = CardType::parse(raw_card_type)?;
    // Invariant: exactly one class, or Neutral.
    let class = CardClass::parse(raw_class)?;
    let rarity = Rarity::parse(raw_rarity)?;

    // Invariant: a boss-locked card must be a Boss-class card.
    if boss_lock.is_some() && class != CardClass::Boss {
        return Err(DomainError::InvariantViolation(
            "a boss-locked card must be of class Boss".to_string(),
        ));
    }

    // Invariant: Juice cost within the legal range for the card's type.
    let (min, max) = card_type.legal_cost_range();
    if cost < min || cost > max {
        return Err(DomainError::InvariantViolation(format!(
            "a {}'s Juice cost must fall within [{min}, {max}]; got {cost}",
            card_type.as_str(),
        )));
    }

    // Invariant: effect-script reference must resolve to a registered effect.
    if !REGISTERED_EFFECTS.contains(&effect_script_ref) {
        return Err(DomainError::InvariantViolation(format!(
            "effect-script reference '{effect_script_ref}' does not resolve to a registered effect"
        )));
    }

    // Invariant: Operators and Vehicles are board units and need a body;
    // Job/Piece/Heist are spell-like and carry no board stats.
    match card_type {
        CardType::Operator | CardType::Vehicle => {
            if hp < 1 {
                return Err(DomainError::InvariantViolation(format!(
                    "a {} is a board unit and must have hp >= 1; got hp {hp}",
                    card_type.as_str(),
                )));
            }
        }
        CardType::Job | CardType::Piece | CardType::Heist => {
            if atk != 0 || hp != 0 {
                return Err(DomainError::InvariantViolation(format!(
                    "a {} is spell-like and must have atk == 0 && hp == 0; got {atk}/{hp}",
                    card_type.as_str(),
                )));
            }
        }
    }

    // Invariant: Legendary rarity carries a per-Outfit copy cap of 1.
    if rarity == Rarity::Legendary && copy_cap != 1 {
        return Err(DomainError::InvariantViolation(format!(
            "Legendary rarity carries a per-Outfit copy cap of 1; definition declared {copy_cap}"
        )));
    }

    Ok(ValidatedCardFields {
        class,
        card_type,
        rarity,
    })
}

/// A validated card definition, produced once every invariant has been checked.
/// Carried by [`Event::CardDefined`] and thus by the emitted `card.defined`
/// event; every field is a proven-legal value object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardDefined {
    pub card_id: String,
    pub name: String,
    pub cost: i64,
    pub class: CardClass,
    pub card_type: CardType,
    pub rarity: Rarity,
    pub keywords: Vec<String>,
    pub effect_script_ref: String,
    pub copy_cap: u32,
    pub atk: u8,
    pub hp: u8,
    pub boss_lock: Option<String>,
}

/// A validated card revision, produced once every invariant has been re-checked
/// against the amended fields. Carried by [`Event::CardRevised`] and thus by the
/// emitted `card.revised` event; every field is a proven-legal value object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardRevised {
    pub card_id: String,
    pub name: String,
    pub cost: i64,
    pub class: CardClass,
    pub card_type: CardType,
    pub rarity: Rarity,
    pub keywords: Vec<String>,
    pub effect_script_ref: String,
    pub copy_cap: u32,
    pub atk: u8,
    pub hp: u8,
    pub boss_lock: Option<String>,
}

/// Domain events emitted by [`CardDefinition`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A card definition passed validation and was added to the catalog.
    CardDefined(CardDefined),
    /// An existing card's amended definition passed re-validation.
    CardRevised(CardRevised),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::CardDefined(_) => "card.defined",
            Event::CardRevised(_) => "card.revised",
        }
    }
}

/// The CardDefinition aggregate: the catalog entry for a single playable card.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the server — is unchanged, while `execute` now handles
/// [`DefineCardCmd`].
#[derive(Debug)]
pub struct CardDefinition {
    id: String,
    root: AggregateRoot,
}

impl CardDefinition {
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

    /// Validate a proposed card definition and, if it holds, record and return
    /// the resulting `card.defined` event.
    ///
    /// Each invariant is checked by parsing the corresponding raw field into a
    /// value object; the first failure short-circuits with a
    /// [`DomainError::InvariantViolation`].
    fn define_card(&mut self, cmd: DefineCardCmd) -> Result<Vec<Event>, DomainError> {
        let ValidatedCardFields {
            class,
            card_type,
            rarity,
        } = validate_card_fields(
            &cmd.name,
            &cmd.card_type,
            &cmd.class,
            &cmd.rarity,
            cmd.cost,
            &cmd.effect_script_ref,
            cmd.copy_cap,
            cmd.atk,
            cmd.hp,
            &cmd.boss_lock,
        )?;

        let event = Event::CardDefined(CardDefined {
            card_id: cmd.card_id,
            name: cmd.name,
            cost: cmd.cost,
            class,
            card_type,
            rarity,
            keywords: cmd.keywords,
            effect_script_ref: cmd.effect_script_ref,
            copy_cap: cmd.copy_cap,
            atk: cmd.atk,
            hp: cmd.hp,
            boss_lock: cmd.boss_lock,
        });

        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Re-validate an amended card definition and, if it holds, record and
    /// return the resulting `card.revised` event.
    ///
    /// A revision submits the full amended definition; it is checked against the
    /// very same catalog invariants as an initial definition (via
    /// [`validate_card_fields`]), so a revision can never move a card into an
    /// illegal state. The first failing invariant short-circuits with a
    /// [`DomainError::InvariantViolation`] and no event is recorded.
    fn revise_card(&mut self, cmd: ReviseCardCmd) -> Result<Vec<Event>, DomainError> {
        let ValidatedCardFields {
            class,
            card_type,
            rarity,
        } = validate_card_fields(
            &cmd.name,
            &cmd.card_type,
            &cmd.class,
            &cmd.rarity,
            cmd.cost,
            &cmd.effect_script_ref,
            cmd.copy_cap,
            cmd.atk,
            cmd.hp,
            &cmd.boss_lock,
        )?;

        let event = Event::CardRevised(CardRevised {
            card_id: cmd.card_id,
            name: cmd.name,
            cost: cmd.cost,
            class,
            card_type,
            rarity,
            keywords: cmd.keywords,
            effect_script_ref: cmd.effect_script_ref,
            copy_cap: cmd.copy_cap,
            atk: cmd.atk,
            hp: cmd.hp,
            boss_lock: cmd.boss_lock,
        });

        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for CardDefinition {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            DEFINE_CARD => {
                let cmd: DefineCardCmd = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed DefineCardCmd payload: {e}"))
                })?;
                self.define_card(cmd)
            }
            REVISE_CARD => {
                let cmd: ReviseCardCmd = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed ReviseCardCmd payload: {e}"))
                })?;
                self.revise_card(cmd)
            }
            _ => Err(DomainError::unknown_command(AGGREGATE_TYPE, command.name)),
        }
    }
}

/// Repository contract for the [`CardDefinition`] aggregate. Adapters implement
/// [`shared::Repository`] for `CardDefinition` and then this marker trait.
pub trait CardDefinitionRepository: Repository<CardDefinition> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `DefineCardCmd` that satisfies every invariant, as a starting point
    /// tests mutate one field at a time to drive a specific rejection.
    fn valid_cmd() -> DefineCardCmd {
        DefineCardCmd {
            card_id: "card-001".to_string(),
            name: "Getaway Driver".to_string(),
            cost: 3,
            class: "Driver".to_string(),
            card_type: "Operator".to_string(),
            rarity: "Common".to_string(),
            keywords: vec!["Fast".to_string()],
            effect_script_ref: "effect.draw_card".to_string(),
            copy_cap: 0,
            atk: 2,
            hp: 2,
            boss_lock: None,
        }
    }

    #[test]
    fn define_card_emits_card_defined_event() {
        let mut agg = CardDefinition::new("card-001");
        let events = agg.execute(valid_cmd().into_command()).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "card.defined");
        assert_eq!(agg.version(), 1);
        assert_eq!(agg.uncommitted_events().len(), 1);
        assert_eq!(agg.uncommitted_events()[0].event_type(), "card.defined");
    }

    #[test]
    fn legendary_card_with_declared_copy_cap_of_one_is_accepted() {
        let mut agg = CardDefinition::new("card-legend");
        let cmd = DefineCardCmd {
            rarity: "Legendary".to_string(),
            copy_cap: 1,
            ..valid_cmd()
        };
        assert!(agg.execute(cmd.into_command()).is_ok());
    }

    #[test]
    fn rejects_cost_outside_type_range() {
        let mut agg = CardDefinition::new("card-001");
        // Operator's legal range is [1, 8]; 9 is out of range.
        let cmd = DefineCardCmd {
            cost: 9,
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    #[test]
    fn rejects_card_claiming_two_classes() {
        let mut agg = CardDefinition::new("card-001");
        let cmd = DefineCardCmd {
            class: "Driver/Hacker".to_string(),
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
    }

    #[test]
    fn rejects_unknown_card_type() {
        let mut agg = CardDefinition::new("card-001");
        let cmd = DefineCardCmd {
            card_type: "Sidekick".to_string(),
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
    }

    #[test]
    fn rejects_unregistered_effect_script_ref() {
        let mut agg = CardDefinition::new("card-001");
        let cmd = DefineCardCmd {
            effect_script_ref: "effect.does_not_exist".to_string(),
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
    }

    #[test]
    fn rejects_legendary_without_copy_cap_of_one() {
        let mut agg = CardDefinition::new("card-001");
        let cmd = DefineCardCmd {
            rarity: "Legendary".to_string(),
            copy_cap: 3,
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
    }

    /// A `ReviseCardCmd` that satisfies every invariant, as a starting point
    /// tests mutate one field at a time to drive a specific rejection.
    fn valid_revise_cmd() -> ReviseCardCmd {
        ReviseCardCmd {
            card_id: "card-001".to_string(),
            name: "Getaway Driver".to_string(),
            cost: 4,
            class: "Driver".to_string(),
            card_type: "Operator".to_string(),
            rarity: "Common".to_string(),
            keywords: vec!["Fast".to_string(), "Nimble".to_string()],
            effect_script_ref: "effect.draw_card".to_string(),
            copy_cap: 0,
            atk: 2,
            hp: 2,
            boss_lock: None,
        }
    }

    #[test]
    fn revise_card_emits_card_revised_event() {
        let mut agg = CardDefinition::new("card-001");
        let events = agg.execute(valid_revise_cmd().into_command()).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "card.revised");
        assert_eq!(agg.version(), 1);
        assert_eq!(agg.uncommitted_events().len(), 1);
        assert_eq!(agg.uncommitted_events()[0].event_type(), "card.revised");
    }

    #[test]
    fn revise_rejects_cost_outside_type_range() {
        let mut agg = CardDefinition::new("card-001");
        // Operator's legal range is [1, 8]; 9 is out of range.
        let cmd = ReviseCardCmd {
            cost: 9,
            ..valid_revise_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    #[test]
    fn revise_rejects_card_claiming_two_classes() {
        let mut agg = CardDefinition::new("card-001");
        let cmd = ReviseCardCmd {
            class: "Driver/Hacker".to_string(),
            ..valid_revise_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
    }

    #[test]
    fn revise_rejects_unknown_card_type() {
        let mut agg = CardDefinition::new("card-001");
        let cmd = ReviseCardCmd {
            card_type: "Sidekick".to_string(),
            ..valid_revise_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
    }

    #[test]
    fn revise_rejects_unregistered_effect_script_ref() {
        let mut agg = CardDefinition::new("card-001");
        let cmd = ReviseCardCmd {
            effect_script_ref: "effect.does_not_exist".to_string(),
            ..valid_revise_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
    }

    #[test]
    fn revise_rejects_legendary_without_copy_cap_of_one() {
        let mut agg = CardDefinition::new("card-001");
        let cmd = ReviseCardCmd {
            rarity: "Legendary".to_string(),
            copy_cap: 3,
            ..valid_revise_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    #[test]
    fn revise_legendary_with_declared_copy_cap_of_one_is_accepted() {
        let mut agg = CardDefinition::new("card-legend");
        let cmd = ReviseCardCmd {
            rarity: "Legendary".to_string(),
            copy_cap: 1,
            ..valid_revise_cmd()
        };
        assert!(agg.execute(cmd.into_command()).is_ok());
    }

    #[test]
    fn operator_requires_positive_hp() {
        let mut agg = CardDefinition::new("card-op");
        // Operator with hp 0 is illegal — a unit needs a body.
        let cmd = DefineCardCmd {
            card_type: "Operator".to_string(),
            atk: 2,
            hp: 0,
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    #[test]
    fn operator_with_hp_is_accepted() {
        let mut agg = CardDefinition::new("card-op");
        let cmd = DefineCardCmd {
            card_type: "Operator".to_string(),
            atk: 2,
            hp: 3,
            ..valid_cmd()
        };
        let events = agg
            .execute(cmd.into_command())
            .expect("a 2/3 Operator is legal");
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::CardDefined(d) => {
                assert_eq!(d.atk, 2);
                assert_eq!(d.hp, 3);
            }
            other => panic!("expected CardDefined, got {other:?}"),
        }
    }

    #[test]
    fn spell_type_rejects_body_stats() {
        let mut agg = CardDefinition::new("card-job");
        // A Job is spell-like: it must have no board body (atk == 0 && hp == 0).
        let cmd = DefineCardCmd {
            card_type: "Job".to_string(),
            atk: 3,
            hp: 0,
            effect_script_ref: "effect.deal_damage".to_string(),
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
    }

    // Scenario: DefineCardCmd rejected — a boss-locked card must be of class
    // Boss (Task 8).
    #[test]
    fn define_card_rejects_boss_lock_on_non_boss_class() {
        let mut agg = CardDefinition::new("card-001");
        // valid_cmd() is class "Driver"; locking it to a Boss is illegal.
        let cmd = DefineCardCmd {
            boss_lock: Some("boss-solomon".to_string()),
            ..valid_cmd()
        };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    // Scenario: a boss-locked card of class Boss is accepted.
    #[test]
    fn boss_locked_card_of_boss_class_is_accepted() {
        let mut agg = CardDefinition::new("card-boss");
        let cmd = DefineCardCmd {
            class: "Boss".to_string(),
            boss_lock: Some("boss-solomon".to_string()),
            ..valid_cmd()
        };
        let events = agg
            .execute(cmd.into_command())
            .expect("a Boss-class boss-locked card is legal");
        match &events[0] {
            Event::CardDefined(d) => {
                assert_eq!(d.boss_lock, Some("boss-solomon".to_string()));
                assert_eq!(d.class, CardClass::Boss);
            }
            other => panic!("expected CardDefined, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_command() {
        let mut agg = CardDefinition::new("card-001");
        let err = agg.execute(Command::new("NoSuchCommand")).unwrap_err();
        assert!(matches!(
            err,
            DomainError::UnknownCommand {
                aggregate: "CardDefinition",
                ..
            }
        ));
    }
}
