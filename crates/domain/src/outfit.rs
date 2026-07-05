//! Outfit bounded context — a player's constructed deck of cards (the
//! collection-and-deckbuilding context).
//!
//! An [`Outfit`] is a single player's deck: the cards drafted into it plus the
//! class the deck is built around. Four invariants govern whether an Outfit is
//! legal for saving/play, and every one of them is re-checked when a card is
//! removed:
//!
//! 1. **Exactly 30 cards** — an Outfit contains exactly 30 cards to be legal
//!    for saving/play; a deck of any other size cannot be saved or played.
//! 2. **Own class or Neutral only** — an Outfit may include only cards of its
//!    own class plus Neutral cards; a card of a foreign class is illegal.
//! 3. **Copy caps** — at most 2 copies of any card (1 copy for a Legendary)
//!    may be included; exceeding the cap is illegal.
//! 4. **Owned at validation time** — every card in the Outfit must be owned in
//!    the player's collection at validation time; an Outfit referencing a card
//!    the player no longer owns is illegal.
//!
//! Three commands are implemented. [`AddCardToOutfit`] (`AddCardToOutfitCmd`)
//! adds a card to the Outfit, enforcing every invariant (copy caps and class
//! legality among them) so the addition keeps the deck legal, and on success
//! emits [`Event::CardAddedToOutfit`] (`card.added.to.outfit`).
//! [`RemoveCardFromOutfit`]
//! (`RemoveCardFromOutfitCmd`) removes a card from the Outfit, enforcing every
//! invariant, and on success emits [`Event::CardRemovedFromOutfit`]
//! (`card.removed.from.outfit`). [`ValidateOutfit`] (`ValidateOutfitCmd`) runs
//! the full 30-card legality validation without mutating the deck — it enforces
//! the same four invariants and, when the Outfit is legal, emits
//! [`Event::OutfitValidated`] (`outfit.validated`). This module is hand-written
//! (it does not use `shared::stub_aggregate!`) but preserves the same public
//! surface — an [`Outfit`] aggregate and an [`OutfitRepository`] port — so any
//! persistence adapters compile against it unchanged, exactly like its sibling
//! [`PlayerCollection`](crate::player_collection).

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "Outfit";

/// The `RemoveCardFromOutfitCmd` command name [`Outfit::execute`] recognizes.
const REMOVE_CARD_FROM_OUTFIT: &str = "RemoveCardFromOutfitCmd";

/// The `AddCardToOutfitCmd` command name [`Outfit::execute`] recognizes.
const ADD_CARD_TO_OUTFIT: &str = "AddCardToOutfitCmd";

/// The `ValidateOutfitCmd` command name [`Outfit::execute`] recognizes.
const VALIDATE_OUTFIT: &str = "ValidateOutfitCmd";

/// The number of cards an Outfit must hold, exactly, to be legal for saving/play.
const LEGAL_OUTFIT_SIZE: i64 = 30;

/// The `RemoveCardFromOutfitCmd` payload: which card is removed from which
/// Outfit. Field names use the deckbuilding service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`RemoveCardFromOutfit::into_command`], or decode it from a command payload
/// via [`serde_json`] inside [`Outfit::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveCardFromOutfit {
    /// The Outfit the card is removed from; must name this Outfit, and must be
    /// non-empty.
    pub outfit_id: String,
    /// The card being removed; must be non-empty.
    pub card_id: String,
}

impl RemoveCardFromOutfit {
    /// The command name this maps to.
    pub const COMMAND: &'static str = REMOVE_CARD_FROM_OUTFIT;

    /// Build a command removing `card_id` from `outfit_id`.
    pub fn new(outfit_id: impl Into<String>, card_id: impl Into<String>) -> Self {
        Self {
            outfit_id: outfit_id.into(),
            card_id: card_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`Outfit::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload =
            serde_json::to_vec(self).expect("RemoveCardFromOutfit is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `AddCardToOutfitCmd` payload: which card is added to which Outfit. Field
/// names use the deckbuilding service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`AddCardToOutfit::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`Outfit::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCardToOutfit {
    /// The Outfit the card is added to; must name this Outfit, and must be
    /// non-empty.
    pub outfit_id: String,
    /// The card being added; must be non-empty.
    pub card_id: String,
}

impl AddCardToOutfit {
    /// The command name this maps to.
    pub const COMMAND: &'static str = ADD_CARD_TO_OUTFIT;

    /// Build a command adding `card_id` to `outfit_id`.
    pub fn new(outfit_id: impl Into<String>, card_id: impl Into<String>) -> Self {
        Self {
            outfit_id: outfit_id.into(),
            card_id: card_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`Outfit::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("AddCardToOutfit is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `ValidateOutfitCmd` payload: which Outfit is being validated. Field
/// names use the deckbuilding service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ValidateOutfit::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`Outfit::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateOutfit {
    /// The Outfit being validated; must name this Outfit, and must be non-empty.
    pub outfit_id: String,
}

impl ValidateOutfit {
    /// The command name this maps to.
    pub const COMMAND: &'static str = VALIDATE_OUTFIT;

    /// Build a command validating `outfit_id`.
    pub fn new(outfit_id: impl Into<String>) -> Self {
        Self {
            outfit_id: outfit_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`Outfit::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ValidateOutfit is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The card that was removed, carried by [`Event::CardRemovedFromOutfit`] and
/// thus by the emitted `card.removed.from.outfit` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardRemovedFromOutfit {
    /// The Outfit the card was removed from.
    pub outfit_id: String,
    /// The card that was removed.
    pub card_id: String,
}

/// The card that was added, carried by [`Event::CardAddedToOutfit`] and thus by
/// the emitted `card.added.to.outfit` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardAddedToOutfit {
    /// The Outfit the card was added to.
    pub outfit_id: String,
    /// The card that was added.
    pub card_id: String,
}

/// The Outfit that passed full legality validation, carried by
/// [`Event::OutfitValidated`] and thus by the emitted `outfit.validated` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutfitValidated {
    /// The Outfit that was validated.
    pub outfit_id: String,
}

/// Domain events emitted by [`Outfit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A card was added to the Outfit.
    CardAddedToOutfit(CardAddedToOutfit),
    /// A card was removed from the Outfit.
    CardRemovedFromOutfit(CardRemovedFromOutfit),
    /// The Outfit passed full 30-card legality validation.
    OutfitValidated(OutfitValidated),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::CardAddedToOutfit(_) => "card.added.to.outfit",
            Event::CardRemovedFromOutfit(_) => "card.removed.from.outfit",
            Event::OutfitValidated(_) => "outfit.validated",
        }
    }
}

/// The Outfit aggregate: one player's constructed deck.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring is unchanged, while it
/// now carries the state the [`RemoveCardFromOutfit`] command validates
/// against: the number of cards in the deck, whether every card is of the
/// Outfit's own class (or Neutral), whether copy caps are respected, and
/// whether every card is owned in the player's collection.
///
/// A fresh Outfit from [`Outfit::new`] is legal and removal-ready: it holds
/// exactly [`LEGAL_OUTFIT_SIZE`] cards, all of its own class or Neutral, all
/// within copy caps, and all owned. The configuration methods below drive it to
/// a state a command rejects, exactly as [`PlayerCollection`](crate::player_collection)
/// is built up before a command validates it.
///
/// The card count is deliberately an `i64` rather than an unsigned type so an
/// *illegal* size (including a negative one) is representable — that is the only
/// way the "exactly 30 cards" invariant can be exercised rather than made
/// vacuous by the type system.
#[derive(Debug)]
pub struct Outfit {
    id: String,
    root: AggregateRoot,
    /// The player who owns this Outfit.
    player_id: String,
    /// The number of cards currently in the deck. Must equal
    /// [`LEGAL_OUTFIT_SIZE`] for the Outfit to be legal for saving/play.
    card_count: i64,
    /// Whether every card in the deck is of the Outfit's own class or Neutral.
    only_own_class_or_neutral: bool,
    /// Whether every card respects its copy cap (≤ 2 copies, ≤ 1 for Legendary).
    within_copy_limits: bool,
    /// Whether every card in the deck is owned in the player's collection at
    /// validation time.
    all_cards_owned: bool,
}

impl Outfit {
    /// Create a new, legal, removal-ready Outfit identified by `id` and owned by
    /// the same player id: it holds exactly [`LEGAL_OUTFIT_SIZE`] cards, all of
    /// its own class or Neutral, all within copy caps, and all owned. Use the
    /// configuration methods to drive it to the state a command validates.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            player_id: id.clone(),
            id,
            root: AggregateRoot::new(),
            card_count: LEGAL_OUTFIT_SIZE,
            only_own_class_or_neutral: true,
            within_copy_limits: true,
            all_cards_owned: true,
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The player who owns this Outfit.
    pub fn player_id(&self) -> &str {
        &self.player_id
    }

    /// The number of cards currently in the deck.
    pub fn card_count(&self) -> i64 {
        self.card_count
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// Set the owning player id (e.g. to test a command naming a different
    /// player).
    pub fn set_player_id(&mut self, player_id: impl Into<String>) {
        self.player_id = player_id.into();
    }

    /// Set the number of cards in the deck.
    pub fn set_card_count(&mut self, card_count: i64) {
        self.card_count = card_count;
    }

    /// Record whether every card is of the Outfit's own class or Neutral.
    pub fn set_only_own_class_or_neutral(&mut self, ok: bool) {
        self.only_own_class_or_neutral = ok;
    }

    /// Record whether every card respects its copy cap.
    pub fn set_within_copy_limits(&mut self, ok: bool) {
        self.within_copy_limits = ok;
    }

    /// Record whether every card in the deck is owned in the player's collection.
    pub fn set_all_cards_owned(&mut self, ok: bool) {
        self.all_cards_owned = ok;
    }

    /// Legal-size invariant: an Outfit contains exactly 30 cards to be legal for
    /// saving/play.
    fn ensure_exactly_thirty(&self) -> Result<(), DomainError> {
        if self.card_count != LEGAL_OUTFIT_SIZE {
            return Err(DomainError::InvariantViolation(format!(
                "outfit '{}' holds {} cards; an Outfit contains exactly {LEGAL_OUTFIT_SIZE} cards \
                 to be legal for saving/play",
                self.id, self.card_count
            )));
        }
        Ok(())
    }

    /// Class invariant: an Outfit may include only cards of its own class plus
    /// Neutral cards.
    fn ensure_only_own_class_or_neutral(&self) -> Result<(), DomainError> {
        if !self.only_own_class_or_neutral {
            return Err(DomainError::InvariantViolation(format!(
                "outfit '{}' includes a card outside its own class; an Outfit may include only \
                 cards of its own class plus Neutral cards",
                self.id
            )));
        }
        Ok(())
    }

    /// Copy-cap invariant: at most 2 copies of any card (1 copy for a Legendary)
    /// may be included.
    fn ensure_within_copy_limits(&self) -> Result<(), DomainError> {
        if !self.within_copy_limits {
            return Err(DomainError::InvariantViolation(format!(
                "outfit '{}' exceeds a card's copy cap; at most 2 copies of any card (1 copy for a \
                 Legendary) may be included",
                self.id
            )));
        }
        Ok(())
    }

    /// Ownership invariant: every card in the Outfit must be owned in the
    /// player's collection at validation time.
    fn ensure_all_cards_owned(&self) -> Result<(), DomainError> {
        if !self.all_cards_owned {
            return Err(DomainError::InvariantViolation(format!(
                "outfit '{}' references a card not owned by player '{}'; every card in the Outfit \
                 must be owned in the player's collection at validation time",
                self.id, self.player_id
            )));
        }
        Ok(())
    }

    /// Handle `AddCardToOutfitCmd`: verify the command carries a valid outfit id
    /// (naming this Outfit) and card id, enforce every invariant (exactly 30
    /// cards, own-class-or-Neutral, copy caps, and owned-at-validation time) so
    /// the addition keeps the deck legal, and emit [`Event::CardAddedToOutfit`].
    fn add_card(&mut self, cmd: AddCardToOutfit) -> Result<Vec<Event>, DomainError> {
        // A valid outfitId and cardId must be supplied.
        if cmd.outfit_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "outfit '{}' requires a valid outfitId to add a card",
                self.id
            )));
        }
        if cmd.card_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "outfit '{}' requires a valid cardId to add a card",
                self.id
            )));
        }
        // The command must name the Outfit it is dispatched to.
        if cmd.outfit_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets outfit '{}' but this aggregate is outfit '{}'",
                cmd.outfit_id, self.id
            )));
        }

        // Enforce every invariant before recording the addition.
        self.ensure_exactly_thirty()?;
        self.ensure_only_own_class_or_neutral()?;
        self.ensure_within_copy_limits()?;
        self.ensure_all_cards_owned()?;

        let event = Event::CardAddedToOutfit(CardAddedToOutfit {
            outfit_id: cmd.outfit_id,
            card_id: cmd.card_id,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `RemoveCardFromOutfitCmd`: verify the command carries a valid
    /// outfit id (naming this Outfit) and card id, enforce every invariant
    /// (exactly 30 cards, own-class-or-Neutral, copy caps, and owned-at-validation
    /// time), and emit [`Event::CardRemovedFromOutfit`].
    fn remove_card(&mut self, cmd: RemoveCardFromOutfit) -> Result<Vec<Event>, DomainError> {
        // A valid outfitId and cardId must be supplied.
        if cmd.outfit_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "outfit '{}' requires a valid outfitId to remove a card",
                self.id
            )));
        }
        if cmd.card_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "outfit '{}' requires a valid cardId to remove a card",
                self.id
            )));
        }
        // The command must name the Outfit it is dispatched to.
        if cmd.outfit_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets outfit '{}' but this aggregate is outfit '{}'",
                cmd.outfit_id, self.id
            )));
        }

        // Enforce every invariant before recording the removal.
        self.ensure_exactly_thirty()?;
        self.ensure_only_own_class_or_neutral()?;
        self.ensure_within_copy_limits()?;
        self.ensure_all_cards_owned()?;

        let event = Event::CardRemovedFromOutfit(CardRemovedFromOutfit {
            outfit_id: cmd.outfit_id,
            card_id: cmd.card_id,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `ValidateOutfitCmd`: verify the command carries a valid outfit id
    /// (naming this Outfit), run the full 30-card legality validation by
    /// enforcing every invariant (exactly 30 cards, own-class-or-Neutral, copy
    /// caps, and owned-at-validation time), and — when the Outfit is legal —
    /// emit [`Event::OutfitValidated`]. This command does not mutate the deck;
    /// it only certifies legality.
    fn validate(&mut self, cmd: ValidateOutfit) -> Result<Vec<Event>, DomainError> {
        // A valid outfitId must be supplied.
        if cmd.outfit_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "outfit '{}' requires a valid outfitId to validate",
                self.id
            )));
        }
        // The command must name the Outfit it is dispatched to.
        if cmd.outfit_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets outfit '{}' but this aggregate is outfit '{}'",
                cmd.outfit_id, self.id
            )));
        }

        // Run the full legality validation before certifying the Outfit.
        self.ensure_exactly_thirty()?;
        self.ensure_only_own_class_or_neutral()?;
        self.ensure_within_copy_limits()?;
        self.ensure_all_cards_owned()?;

        let event = Event::OutfitValidated(OutfitValidated {
            outfit_id: cmd.outfit_id,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for Outfit {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            ADD_CARD_TO_OUTFIT => {
                let cmd: AddCardToOutfit =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed AddCardToOutfitCmd payload: {e}"
                        ))
                    })?;
                self.add_card(cmd)
            }
            REMOVE_CARD_FROM_OUTFIT => {
                let cmd: RemoveCardFromOutfit =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed RemoveCardFromOutfitCmd payload: {e}"
                        ))
                    })?;
                self.remove_card(cmd)
            }
            VALIDATE_OUTFIT => {
                let cmd: ValidateOutfit =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ValidateOutfitCmd payload: {e}"
                        ))
                    })?;
                self.validate(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`Outfit`] aggregate. Adapters implement
/// [`shared::Repository`] for `Outfit` and then this marker trait.
pub trait OutfitRepository: Repository<Outfit> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A legal, removal-ready Outfit `o-01` owned by `o-01`: exactly 30 cards,
    /// all of its own class or Neutral, all within copy caps, all owned. Tests
    /// mutate one aspect at a time to drive a specific rejection.
    fn ready_outfit() -> Outfit {
        let mut outfit = Outfit::new("o-01");
        outfit.set_card_count(LEGAL_OUTFIT_SIZE);
        outfit.set_only_own_class_or_neutral(true);
        outfit.set_within_copy_limits(true);
        outfit.set_all_cards_owned(true);
        outfit
    }

    /// A command removing card `c-01` from outfit `o-01`.
    fn valid_cmd() -> RemoveCardFromOutfit {
        RemoveCardFromOutfit::new("o-01", "c-01")
    }

    // Scenario: Successfully execute RemoveCardFromOutfitCmd.
    #[test]
    fn removes_and_emits_card_removed_event() {
        let mut outfit = ready_outfit();

        let events = outfit
            .execute(valid_cmd().into_command())
            .expect("valid removal should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "card.removed.from.outfit");
        match &events[0] {
            Event::CardRemovedFromOutfit(removed) => {
                assert_eq!(removed.outfit_id, "o-01");
                assert_eq!(removed.card_id, "c-01");
            }
            other => panic!("expected CardRemovedFromOutfit, got {other:?}"),
        }
        // The Outfit recorded the event.
        assert_eq!(outfit.version(), 1);
        assert_eq!(outfit.uncommitted_events().len(), 1);
        assert_eq!(
            outfit.uncommitted_events()[0].event_type(),
            "card.removed.from.outfit"
        );
    }

    // Scenario: rejected — an Outfit contains exactly 30 cards to be legal for
    // saving/play.
    #[test]
    fn rejects_when_not_exactly_thirty_cards() {
        let mut outfit = ready_outfit();
        // A deck of any size other than 30 is illegal for saving/play.
        outfit.set_card_count(29);

        let err = outfit
            .execute(valid_cmd().into_command())
            .expect_err("an Outfit that is not exactly 30 cards must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Scenario: rejected — an Outfit may include only cards of its own class plus
    // Neutral cards.
    #[test]
    fn rejects_when_card_outside_own_class() {
        let mut outfit = ready_outfit();
        // The deck includes a card of a foreign class.
        outfit.set_only_own_class_or_neutral(false);

        let err = outfit
            .execute(valid_cmd().into_command())
            .expect_err("an Outfit with a foreign-class card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Scenario: rejected — at most 2 copies of any card (1 copy for Legendary)
    // may be included.
    #[test]
    fn rejects_when_copy_cap_exceeded() {
        let mut outfit = ready_outfit();
        // The deck exceeds a card's copy cap.
        outfit.set_within_copy_limits(false);

        let err = outfit
            .execute(valid_cmd().into_command())
            .expect_err("an Outfit exceeding a copy cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Scenario: rejected — every card in the Outfit must be owned in the player's
    // collection at validation time.
    #[test]
    fn rejects_when_card_not_owned() {
        let mut outfit = ready_outfit();
        // The deck references a card the player does not own.
        outfit.set_all_cards_owned(false);

        let err = outfit
            .execute(valid_cmd().into_command())
            .expect_err("an Outfit referencing an unowned card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // A command naming a different Outfit is rejected before any invariant runs.
    #[test]
    fn rejects_command_for_a_different_outfit() {
        let mut outfit = ready_outfit();
        let cmd = RemoveCardFromOutfit::new("o-99", "c-01");

        let err = outfit
            .execute(cmd.into_command())
            .expect_err("a command for another outfit must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Commands missing any required field are rejected.
    #[test]
    fn rejects_command_with_missing_fields() {
        for cmd in [
            RemoveCardFromOutfit::new("   ", "c-01"),
            RemoveCardFromOutfit::new("o-01", "   "),
        ] {
            let mut outfit = ready_outfit();
            let err = outfit
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(outfit.version(), 0);
        }
    }

    /// A command validating outfit `o-01`.
    fn valid_validate_cmd() -> ValidateOutfit {
        ValidateOutfit::new("o-01")
    }

    // Scenario: Successfully execute ValidateOutfitCmd.
    #[test]
    fn validates_and_emits_outfit_validated_event() {
        let mut outfit = ready_outfit();

        let events = outfit
            .execute(valid_validate_cmd().into_command())
            .expect("valid validation should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "outfit.validated");
        match &events[0] {
            Event::OutfitValidated(validated) => {
                assert_eq!(validated.outfit_id, "o-01");
            }
            other => panic!("expected OutfitValidated, got {other:?}"),
        }
        // The Outfit recorded the event.
        assert_eq!(outfit.version(), 1);
        assert_eq!(outfit.uncommitted_events().len(), 1);
        assert_eq!(
            outfit.uncommitted_events()[0].event_type(),
            "outfit.validated"
        );
    }

    // Scenario: ValidateOutfitCmd rejected — an Outfit contains exactly 30 cards
    // to be legal for saving/play.
    #[test]
    fn validate_rejects_when_not_exactly_thirty_cards() {
        let mut outfit = ready_outfit();
        // A deck of any size other than 30 is illegal for saving/play.
        outfit.set_card_count(31);

        let err = outfit
            .execute(valid_validate_cmd().into_command())
            .expect_err("an Outfit that is not exactly 30 cards must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Scenario: ValidateOutfitCmd rejected — an Outfit may include only cards of
    // its own class plus Neutral cards.
    #[test]
    fn validate_rejects_when_card_outside_own_class() {
        let mut outfit = ready_outfit();
        // The deck includes a card of a foreign class.
        outfit.set_only_own_class_or_neutral(false);

        let err = outfit
            .execute(valid_validate_cmd().into_command())
            .expect_err("an Outfit with a foreign-class card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Scenario: ValidateOutfitCmd rejected — at most 2 copies of any card (1 copy
    // for Legendary) may be included.
    #[test]
    fn validate_rejects_when_copy_cap_exceeded() {
        let mut outfit = ready_outfit();
        // The deck exceeds a card's copy cap.
        outfit.set_within_copy_limits(false);

        let err = outfit
            .execute(valid_validate_cmd().into_command())
            .expect_err("an Outfit exceeding a copy cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Scenario: ValidateOutfitCmd rejected — every card in the Outfit must be
    // owned in the player's collection at validation time.
    #[test]
    fn validate_rejects_when_card_not_owned() {
        let mut outfit = ready_outfit();
        // The deck references a card the player does not own.
        outfit.set_all_cards_owned(false);

        let err = outfit
            .execute(valid_validate_cmd().into_command())
            .expect_err("an Outfit referencing an unowned card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // A validate command naming a different Outfit is rejected before any
    // invariant runs.
    #[test]
    fn validate_rejects_command_for_a_different_outfit() {
        let mut outfit = ready_outfit();
        let cmd = ValidateOutfit::new("o-99");

        let err = outfit
            .execute(cmd.into_command())
            .expect_err("a command for another outfit must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // A validate command with a missing outfitId is rejected.
    #[test]
    fn validate_rejects_command_with_missing_outfit_id() {
        let mut outfit = ready_outfit();
        let err = outfit
            .execute(ValidateOutfit::new("   ").into_command())
            .expect_err("a command with a missing outfitId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    #[test]
    fn validate_command_payload_round_trips() {
        let cmd = valid_validate_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, ValidateOutfit::COMMAND);
        let decoded: ValidateOutfit = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_validate_cmd());
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut outfit = Outfit::new("o-01");
        let err = outfit.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "Outfit");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, RemoveCardFromOutfit::COMMAND);
        let decoded: RemoveCardFromOutfit = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }

    /// A command adding card `c-01` to outfit `o-01`.
    fn valid_add_cmd() -> AddCardToOutfit {
        AddCardToOutfit::new("o-01", "c-01")
    }

    // Scenario: Successfully execute AddCardToOutfitCmd.
    #[test]
    fn adds_and_emits_card_added_event() {
        let mut outfit = ready_outfit();

        let events = outfit
            .execute(valid_add_cmd().into_command())
            .expect("valid addition should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "card.added.to.outfit");
        match &events[0] {
            Event::CardAddedToOutfit(added) => {
                assert_eq!(added.outfit_id, "o-01");
                assert_eq!(added.card_id, "c-01");
            }
            other => panic!("expected CardAddedToOutfit, got {other:?}"),
        }
        // The Outfit recorded the event.
        assert_eq!(outfit.version(), 1);
        assert_eq!(outfit.uncommitted_events().len(), 1);
        assert_eq!(
            outfit.uncommitted_events()[0].event_type(),
            "card.added.to.outfit"
        );
    }

    // Scenario: AddCardToOutfitCmd rejected — an Outfit contains exactly 30 cards
    // to be legal for saving/play.
    #[test]
    fn add_rejects_when_not_exactly_thirty_cards() {
        let mut outfit = ready_outfit();
        // A deck of any size other than 30 is illegal for saving/play.
        outfit.set_card_count(31);

        let err = outfit
            .execute(valid_add_cmd().into_command())
            .expect_err("an Outfit that is not exactly 30 cards must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Scenario: AddCardToOutfitCmd rejected — an Outfit may include only cards of
    // its own class plus Neutral cards.
    #[test]
    fn add_rejects_when_card_outside_own_class() {
        let mut outfit = ready_outfit();
        // The deck includes a card of a foreign class.
        outfit.set_only_own_class_or_neutral(false);

        let err = outfit
            .execute(valid_add_cmd().into_command())
            .expect_err("an Outfit with a foreign-class card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Scenario: AddCardToOutfitCmd rejected — at most 2 copies of any card (1 copy
    // for Legendary) may be included.
    #[test]
    fn add_rejects_when_copy_cap_exceeded() {
        let mut outfit = ready_outfit();
        // The deck exceeds a card's copy cap.
        outfit.set_within_copy_limits(false);

        let err = outfit
            .execute(valid_add_cmd().into_command())
            .expect_err("an Outfit exceeding a copy cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Scenario: AddCardToOutfitCmd rejected — every card in the Outfit must be
    // owned in the player's collection at validation time.
    #[test]
    fn add_rejects_when_card_not_owned() {
        let mut outfit = ready_outfit();
        // The deck references a card the player does not own.
        outfit.set_all_cards_owned(false);

        let err = outfit
            .execute(valid_add_cmd().into_command())
            .expect_err("an Outfit referencing an unowned card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // An add command naming a different Outfit is rejected before any invariant
    // runs.
    #[test]
    fn add_rejects_command_for_a_different_outfit() {
        let mut outfit = ready_outfit();
        let cmd = AddCardToOutfit::new("o-99", "c-01");

        let err = outfit
            .execute(cmd.into_command())
            .expect_err("a command for another outfit must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(outfit.version(), 0);
    }

    // Add commands missing any required field are rejected.
    #[test]
    fn add_rejects_command_with_missing_fields() {
        for cmd in [
            AddCardToOutfit::new("   ", "c-01"),
            AddCardToOutfit::new("o-01", "   "),
        ] {
            let mut outfit = ready_outfit();
            let err = outfit
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(outfit.version(), 0);
        }
    }

    #[test]
    fn add_command_payload_round_trips() {
        let cmd = valid_add_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, AddCardToOutfit::COMMAND);
        let decoded: AddCardToOutfit = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_add_cmd());
    }
}
