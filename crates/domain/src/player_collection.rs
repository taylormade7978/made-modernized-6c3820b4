//! PlayerCollection bounded context — a player's owned cards and the cosmetic
//! skins equipped onto them (the collection-and-deckbuilding context).
//!
//! A [`PlayerCollection`] is the set of cards a single player owns, together
//! with the cosmetic skins the player has equipped onto those cards. Four
//! invariants govern equipping a cosmetic skin onto a base card:
//!
//! 1. **Server-authoritative equips** — cosmetic equips are resolved
//!    server-side and are *never trusted from the client*; an equip request that
//!    was asserted by the client rather than resolved by the authoritative
//!    server is rejected.
//! 2. **Owned base card** — a cosmetic skin may only be equipped onto a base
//!    card the player actually owns; equipping onto a card absent from the
//!    collection is inconsistent.
//! 3. **Non-negative quantities** — owned card quantities are always
//!    non-negative; a card recorded with a negative quantity is a corrupt state.
//! 4. **Present for inclusion (qty ≥ 1)** — a card may only be included in an
//!    Outfit if it is present (quantity ≥ 1) in the collection; a base card at
//!    quantity zero cannot carry a cosmetic.
//!
//! Three commands are implemented:
//!
//! - [`EquipCosmetic`] (`EquipCosmeticCmd`): equips a cosmetic skin onto an
//!   owned base card, enforcing every invariant, and on success emits
//!   [`Event::CosmeticEquipped`] (`cosmetic.equipped`).
//! - [`UnequipCosmetic`] (`UnequipCosmeticCmd`): removes the equipped cosmetic
//!   from an owned base card, enforcing every invariant, and on success emits
//!   [`Event::CosmeticUnequipped`] (`cosmetic.unequipped`).
//! - [`GrantCards`] (`GrantCardsCmd`): adds cards to the collection from packs,
//!   rewards, or fulfillment, enforcing every invariant, and on success emits
//!   [`Event::CardsGranted`] (`cards.granted`).
//!
//! This module is hand-written (it does not use `shared::stub_aggregate!`) but
//! preserves the same public surface — a [`PlayerCollection`] aggregate and a
//! [`PlayerCollectionRepository`] port — so the persistence adapters in
//! `crates/mocks` compile against it unchanged.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "PlayerCollection";

/// The command names [`PlayerCollection::execute`] recognizes.
const EQUIP_COSMETIC: &str = "EquipCosmeticCmd";
const UNEQUIP_COSMETIC: &str = "UnequipCosmeticCmd";
const GRANT_CARDS: &str = "GrantCardsCmd";

/// The `EquipCosmeticCmd` payload: which player equips which cosmetic skin onto
/// which owned base card. Field names use the collection service's `camelCase`
/// schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`EquipCosmetic::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`PlayerCollection::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EquipCosmetic {
    /// Identity of the player equipping the skin; must name the player this
    /// collection belongs to, and must be non-empty.
    pub player_id: String,
    /// The owned base card the cosmetic skin is equipped onto; must be non-empty.
    pub base_card_id: String,
    /// Reference to the cosmetic skin being equipped; must be non-empty.
    pub cosmetic_skin_ref: String,
}

impl EquipCosmetic {
    /// The command name this maps to.
    pub const COMMAND: &'static str = EQUIP_COSMETIC;

    /// Build a command equipping `cosmetic_skin_ref` onto `base_card_id` for
    /// `player_id`.
    pub fn new(
        player_id: impl Into<String>,
        base_card_id: impl Into<String>,
        cosmetic_skin_ref: impl Into<String>,
    ) -> Self {
        Self {
            player_id: player_id.into(),
            base_card_id: base_card_id.into(),
            cosmetic_skin_ref: cosmetic_skin_ref.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`PlayerCollection::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("EquipCosmetic is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `UnequipCosmeticCmd` payload: which player removes the equipped cosmetic
/// skin from which owned base card. Field names use the collection service's
/// `camelCase` schema.
///
/// Unlike [`EquipCosmetic`] there is no `cosmeticSkinRef`: unequipping clears
/// whatever cosmetic the base card currently carries, so only the owning player
/// and the target base card need to be named.
///
/// Build one directly and turn it into a [`Command`] with
/// [`UnequipCosmetic::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`PlayerCollection::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnequipCosmetic {
    /// Identity of the player removing the skin; must name the player this
    /// collection belongs to, and must be non-empty.
    pub player_id: String,
    /// The owned base card the cosmetic skin is removed from; must be non-empty.
    pub base_card_id: String,
}

impl UnequipCosmetic {
    /// The command name this maps to.
    pub const COMMAND: &'static str = UNEQUIP_COSMETIC;

    /// Build a command removing the equipped cosmetic from `base_card_id` for
    /// `player_id`.
    pub fn new(player_id: impl Into<String>, base_card_id: impl Into<String>) -> Self {
        Self {
            player_id: player_id.into(),
            base_card_id: base_card_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`PlayerCollection::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("UnequipCosmetic is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// A single line item in a [`GrantCards`] command: how many copies of one card
/// are being added to the collection. Field names use the collection service's
/// `camelCase` schema.
///
/// The quantity is an `i64` rather than an unsigned type for the same reason the
/// aggregate's owned quantity is: so a *negative* granted quantity is
/// representable and the "owned card quantities are always non-negative"
/// invariant can be exercised rather than made vacuous by the type system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardGrant {
    /// The card being granted; must be non-empty.
    pub card_id: String,
    /// How many copies are granted; must be non-negative.
    pub quantity: i64,
}

impl CardGrant {
    /// Build a grant of `quantity` copies of `card_id`.
    pub fn new(card_id: impl Into<String>, quantity: i64) -> Self {
        Self {
            card_id: card_id.into(),
            quantity,
        }
    }
}

/// The `GrantCardsCmd` payload: which player receives which cards, and the
/// source the grant was resolved from (a pack opening, a reward, or a
/// fulfillment). Field names use the collection service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`GrantCards::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`PlayerCollection::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantCards {
    /// Identity of the player receiving the cards; must name the player this
    /// collection belongs to, and must be non-empty.
    pub player_id: String,
    /// The cards being added to the collection; must be non-empty, and every
    /// grant must carry a non-empty card id and a non-negative quantity.
    pub card_grants: Vec<CardGrant>,
    /// Where the grant came from (e.g. `"pack"`, `"reward"`, `"fulfillment"`);
    /// must be non-empty.
    pub source: String,
}

impl GrantCards {
    /// The command name this maps to.
    pub const COMMAND: &'static str = GRANT_CARDS;

    /// Build a command granting `card_grants` to `player_id` from `source`.
    pub fn new(
        player_id: impl Into<String>,
        card_grants: Vec<CardGrant>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            player_id: player_id.into(),
            card_grants,
            source: source.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`PlayerCollection::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("GrantCards is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The cosmetic that was equipped, carried by [`Event::CosmeticEquipped`] and
/// thus by the emitted `cosmetic.equipped` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CosmeticEquipped {
    /// The player who equipped the skin.
    pub player_id: String,
    /// The owned base card the skin was equipped onto.
    pub base_card_id: String,
    /// Reference to the cosmetic skin that was equipped.
    pub cosmetic_skin_ref: String,
}

/// The cosmetic that was removed, carried by [`Event::CosmeticUnequipped`] and
/// thus by the emitted `cosmetic.unequipped` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CosmeticUnequipped {
    /// The player who removed the skin.
    pub player_id: String,
    /// The owned base card the skin was removed from.
    pub base_card_id: String,
}

/// The cards that were granted, carried by [`Event::CardsGranted`] and thus by
/// the emitted `cards.granted` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardsGranted {
    /// The player who received the cards.
    pub player_id: String,
    /// The cards that were added to the collection.
    pub card_grants: Vec<CardGrant>,
    /// Where the grant came from (e.g. `"pack"`, `"reward"`, `"fulfillment"`).
    pub source: String,
}

/// Domain events emitted by [`PlayerCollection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A cosmetic skin was equipped onto an owned base card.
    CosmeticEquipped(CosmeticEquipped),
    /// The equipped cosmetic skin was removed from an owned base card.
    CosmeticUnequipped(CosmeticUnequipped),
    /// Cards were granted to the collection from a pack, reward, or fulfillment.
    CardsGranted(CardsGranted),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::CosmeticEquipped(_) => "cosmetic.equipped",
            Event::CosmeticUnequipped(_) => "cosmetic.unequipped",
            Event::CardsGranted(_) => "cards.granted",
        }
    }
}

/// The PlayerCollection aggregate: one player's owned cards and equipped skins.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the server — is unchanged, while it now carries the
/// state the [`EquipCosmetic`] command validates against: the owning player, and
/// for the base card an equip targets, whether that card is in the collection,
/// its owned quantity, and whether the equip was resolved server-side.
///
/// A fresh collection from [`PlayerCollection::new`] is equip-ready: the equip is
/// server-resolved, the targeted base card is in the collection, and it is owned
/// at quantity one. The configuration methods below drive it to a state a command
/// rejects, exactly as [`Season`](crate::season) is built up before a command
/// validates it.
///
/// The owned quantity is deliberately an `i64` rather than an unsigned type so a
/// *negative* quantity is representable — that is the only way the
/// "quantities are always non-negative" invariant can be exercised rather than
/// made vacuous by the type system.
#[derive(Debug)]
pub struct PlayerCollection {
    id: String,
    root: AggregateRoot,
    /// The player who owns this collection; an equip must name this player.
    player_id: String,
    /// Whether the base card an equip targets is present in the collection at
    /// all (an entry exists for it). The player must own the base card being
    /// skinned.
    base_card_in_collection: bool,
    /// The owned quantity of the base card an equip targets. Must be
    /// non-negative, and ≥ 1 for the card to carry a cosmetic.
    base_card_quantity: i64,
    /// Whether the equip was resolved by the authoritative server. Client-
    /// asserted equips are never trusted.
    server_resolved: bool,
}

impl PlayerCollection {
    /// Create a new, equip-ready collection identified by `id` and owned by the
    /// same player id: the equip is server-resolved, the targeted base card is in
    /// the collection, and it is owned at quantity one. Use the configuration
    /// methods to drive it to the state a command validates.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            player_id: id.clone(),
            id,
            root: AggregateRoot::new(),
            base_card_in_collection: true,
            base_card_quantity: 1,
            server_resolved: true,
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The player who owns this collection.
    pub fn player_id(&self) -> &str {
        &self.player_id
    }

    /// The owned quantity of the base card an equip targets.
    pub fn base_card_quantity(&self) -> i64 {
        self.base_card_quantity
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

    /// Record whether the targeted base card is present in the collection at all.
    pub fn set_base_card_in_collection(&mut self, in_collection: bool) {
        self.base_card_in_collection = in_collection;
    }

    /// Set the owned quantity of the targeted base card.
    pub fn set_base_card_quantity(&mut self, quantity: i64) {
        self.base_card_quantity = quantity;
    }

    /// Record whether the equip was resolved server-side (vs. client-asserted).
    pub fn set_server_resolved(&mut self, server_resolved: bool) {
        self.server_resolved = server_resolved;
    }

    /// Server-authoritative invariant: cosmetic equips are resolved server-side
    /// and never trusted from the client.
    fn ensure_server_resolved(&self) -> Result<(), DomainError> {
        if !self.server_resolved {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' received a client-asserted cosmetic equip; equips are resolved \
                 server-side and never trusted from the client",
                self.id
            )));
        }
        Ok(())
    }

    /// Owned-base-card invariant: a cosmetic skin may only be equipped onto a
    /// base card the player actually owns.
    fn ensure_owns_base_card(&self, base_card_id: &str) -> Result<(), DomainError> {
        if !self.base_card_in_collection {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' does not own base card '{base_card_id}'; a cosmetic skin may only \
                 be equipped onto a base card the player actually owns",
                self.id
            )));
        }
        Ok(())
    }

    /// Non-negative-quantity invariant: owned card quantities are always
    /// non-negative.
    fn ensure_quantity_non_negative(&self, base_card_id: &str) -> Result<(), DomainError> {
        if self.base_card_quantity < 0 {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' records base card '{base_card_id}' at quantity {}; owned card \
                 quantities are always non-negative",
                self.id, self.base_card_quantity
            )));
        }
        Ok(())
    }

    /// Present-for-inclusion invariant: a card may only be included in an Outfit
    /// if it is present (quantity ≥ 1) in the collection.
    fn ensure_present_for_inclusion(&self, base_card_id: &str) -> Result<(), DomainError> {
        if self.base_card_quantity < 1 {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' holds base card '{base_card_id}' at quantity {}; a card may only \
                 be included in an Outfit if it is present (qty ≥ 1)",
                self.id, self.base_card_quantity
            )));
        }
        Ok(())
    }

    /// Handle `EquipCosmeticCmd`: verify the command carries a valid player id
    /// (naming this collection's player), base card id, and cosmetic skin ref,
    /// enforce every invariant (server-authoritative, owned base card,
    /// non-negative quantity, and present-for-inclusion), and emit
    /// [`Event::CosmeticEquipped`].
    fn equip_cosmetic(&mut self, cmd: EquipCosmetic) -> Result<Vec<Event>, DomainError> {
        // A valid playerId, baseCardId, and cosmeticSkinRef must be supplied.
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' requires a valid playerId to equip a cosmetic",
                self.id
            )));
        }
        if cmd.base_card_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' requires a valid baseCardId to equip a cosmetic",
                self.id
            )));
        }
        if cmd.cosmetic_skin_ref.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' requires a valid cosmeticSkinRef to equip a cosmetic",
                self.id
            )));
        }
        // The command must name the player this collection actually belongs to.
        if cmd.player_id != self.player_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets player '{}' but this collection belongs to '{}'",
                cmd.player_id, self.player_id
            )));
        }

        // Enforce every invariant before recording the equip.
        self.ensure_server_resolved()?;
        self.ensure_owns_base_card(&cmd.base_card_id)?;
        self.ensure_quantity_non_negative(&cmd.base_card_id)?;
        self.ensure_present_for_inclusion(&cmd.base_card_id)?;

        let event = Event::CosmeticEquipped(CosmeticEquipped {
            player_id: cmd.player_id,
            base_card_id: cmd.base_card_id,
            cosmetic_skin_ref: cmd.cosmetic_skin_ref,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `UnequipCosmeticCmd`: verify the command carries a valid player id
    /// (naming this collection's player) and base card id, enforce every
    /// invariant (server-authoritative, owned base card, non-negative quantity,
    /// and present-for-inclusion), and emit [`Event::CosmeticUnequipped`].
    fn unequip_cosmetic(&mut self, cmd: UnequipCosmetic) -> Result<Vec<Event>, DomainError> {
        // A valid playerId and baseCardId must be supplied.
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' requires a valid playerId to unequip a cosmetic",
                self.id
            )));
        }
        if cmd.base_card_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' requires a valid baseCardId to unequip a cosmetic",
                self.id
            )));
        }
        // The command must name the player this collection actually belongs to.
        if cmd.player_id != self.player_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets player '{}' but this collection belongs to '{}'",
                cmd.player_id, self.player_id
            )));
        }

        // Enforce every invariant before recording the unequip.
        self.ensure_server_resolved()?;
        self.ensure_owns_base_card(&cmd.base_card_id)?;
        self.ensure_quantity_non_negative(&cmd.base_card_id)?;
        self.ensure_present_for_inclusion(&cmd.base_card_id)?;

        let event = Event::CosmeticUnequipped(CosmeticUnequipped {
            player_id: cmd.player_id,
            base_card_id: cmd.base_card_id,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `GrantCardsCmd`: verify the command carries a valid player id
    /// (naming this collection's player), a source, and at least one well-formed
    /// card grant, enforce every collection invariant (server-authoritative,
    /// owned base card, non-negative quantity, and present-for-inclusion), and
    /// emit [`Event::CardsGranted`].
    fn grant_cards(&mut self, cmd: GrantCards) -> Result<Vec<Event>, DomainError> {
        // A valid playerId, source, and at least one cardGrant must be supplied.
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' requires a valid playerId to grant cards",
                self.id
            )));
        }
        if cmd.source.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' requires a valid source to grant cards",
                self.id
            )));
        }
        if cmd.card_grants.is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "collection '{}' requires at least one cardGrant to grant cards",
                self.id
            )));
        }
        for grant in &cmd.card_grants {
            if grant.card_id.trim().is_empty() {
                return Err(DomainError::InvariantViolation(format!(
                    "collection '{}' requires a valid cardId in every cardGrant",
                    self.id
                )));
            }
            // Owned card quantities are always non-negative — a client may not
            // grant a negative quantity.
            if grant.quantity < 0 {
                return Err(DomainError::InvariantViolation(format!(
                    "collection '{}' received cardGrant for '{}' at quantity {}; owned card \
                     quantities are always non-negative",
                    self.id, grant.card_id, grant.quantity
                )));
            }
        }
        // The command must name the player this collection actually belongs to.
        if cmd.player_id != self.player_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets player '{}' but this collection belongs to '{}'",
                cmd.player_id, self.player_id
            )));
        }

        // Enforce every standing collection invariant before recording the grant.
        self.ensure_server_resolved()?;
        self.ensure_owns_base_card(&cmd.card_grants[0].card_id)?;
        self.ensure_quantity_non_negative(&cmd.card_grants[0].card_id)?;
        self.ensure_present_for_inclusion(&cmd.card_grants[0].card_id)?;

        let event = Event::CardsGranted(CardsGranted {
            player_id: cmd.player_id,
            card_grants: cmd.card_grants,
            source: cmd.source,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for PlayerCollection {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            EQUIP_COSMETIC => {
                let cmd: EquipCosmetic = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!(
                        "malformed EquipCosmeticCmd payload: {e}"
                    ))
                })?;
                self.equip_cosmetic(cmd)
            }
            UNEQUIP_COSMETIC => {
                let cmd: UnequipCosmetic =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed UnequipCosmeticCmd payload: {e}"
                        ))
                    })?;
                self.unequip_cosmetic(cmd)
            }
            GRANT_CARDS => {
                let cmd: GrantCards = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed GrantCardsCmd payload: {e}"))
                })?;
                self.grant_cards(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`PlayerCollection`] aggregate. Adapters implement
/// [`shared::Repository`] for `PlayerCollection` and then this marker trait.
pub trait PlayerCollectionRepository: Repository<PlayerCollection> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// An equip-ready collection `p-01`: server-resolved, owning base card
    /// `c-01` at quantity one. Tests mutate one aspect at a time to drive a
    /// specific rejection.
    fn ready_collection() -> PlayerCollection {
        let mut collection = PlayerCollection::new("p-01");
        collection.set_player_id("p-01");
        collection.set_server_resolved(true);
        collection.set_base_card_in_collection(true);
        collection.set_base_card_quantity(1);
        collection
    }

    /// A command equipping skin `skin-neon` onto base card `c-01` for `p-01`.
    fn valid_cmd() -> EquipCosmetic {
        EquipCosmetic::new("p-01", "c-01", "skin-neon")
    }

    // Scenario: Successfully execute EquipCosmeticCmd.
    #[test]
    fn equips_and_emits_cosmetic_equipped_event() {
        let mut collection = ready_collection();

        let events = collection
            .execute(valid_cmd().into_command())
            .expect("valid equip should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "cosmetic.equipped");
        match &events[0] {
            Event::CosmeticEquipped(equipped) => {
                assert_eq!(equipped.player_id, "p-01");
                assert_eq!(equipped.base_card_id, "c-01");
                assert_eq!(equipped.cosmetic_skin_ref, "skin-neon");
            }
            other => panic!("expected CosmeticEquipped, got {other:?}"),
        }
        // The collection recorded the event.
        assert_eq!(collection.version(), 1);
        assert_eq!(collection.uncommitted_events().len(), 1);
        assert_eq!(
            collection.uncommitted_events()[0].event_type(),
            "cosmetic.equipped"
        );
    }

    // Scenario: rejected — a card may only be included in an Outfit if it is
    // present (qty ≥ 1) in the collection.
    #[test]
    fn rejects_when_base_card_not_present() {
        let mut collection = ready_collection();
        // Owned, but at quantity zero — present-for-inclusion requires qty ≥ 1.
        collection.set_base_card_quantity(0);

        let err = collection
            .execute(valid_cmd().into_command())
            .expect_err("an at-quantity-zero base card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Scenario: rejected — owned card quantities are always non-negative.
    #[test]
    fn rejects_when_quantity_negative() {
        let mut collection = ready_collection();
        // A negative owned quantity is a corrupt state.
        collection.set_base_card_quantity(-1);

        let err = collection
            .execute(valid_cmd().into_command())
            .expect_err("a negative owned quantity must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Scenario: rejected — a cosmetic skin may only be equipped onto a base card
    // the player actually owns.
    #[test]
    fn rejects_when_base_card_not_owned() {
        let mut collection = ready_collection();
        // The base card the skin targets is not in the collection at all.
        collection.set_base_card_in_collection(false);

        let err = collection
            .execute(valid_cmd().into_command())
            .expect_err("equipping onto an unowned base card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Scenario: rejected — cosmetic equips are resolved server-side and never
    // trusted from the client.
    #[test]
    fn rejects_when_equip_client_asserted() {
        let mut collection = ready_collection();
        // The equip was asserted by the client, not resolved server-side.
        collection.set_server_resolved(false);

        let err = collection
            .execute(valid_cmd().into_command())
            .expect_err("a client-asserted equip must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // A command naming a different player is rejected before any invariant runs.
    #[test]
    fn rejects_command_for_a_different_player() {
        let mut collection = ready_collection();
        let cmd = EquipCosmetic::new("p-99", "c-01", "skin-neon");

        let err = collection
            .execute(cmd.into_command())
            .expect_err("a command for another player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Commands missing any required field are rejected.
    #[test]
    fn rejects_command_with_missing_fields() {
        for cmd in [
            EquipCosmetic::new("   ", "c-01", "skin-neon"),
            EquipCosmetic::new("p-01", "   ", "skin-neon"),
            EquipCosmetic::new("p-01", "c-01", "   "),
        ] {
            let mut collection = ready_collection();
            let err = collection
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(collection.version(), 0);
        }
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut collection = PlayerCollection::new("p-01");
        let err = collection
            .execute(Command::new("NoSuchCommand"))
            .unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "PlayerCollection");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, EquipCosmetic::COMMAND);
        let decoded: EquipCosmetic = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }

    // ---- UnequipCosmeticCmd (S-38) ----------------------------------------

    /// A command removing the equipped cosmetic from base card `c-01` for `p-01`.
    fn valid_unequip_cmd() -> UnequipCosmetic {
        UnequipCosmetic::new("p-01", "c-01")
    }

    // Scenario: Successfully execute UnequipCosmeticCmd.
    #[test]
    fn unequips_and_emits_cosmetic_unequipped_event() {
        let mut collection = ready_collection();

        let events = collection
            .execute(valid_unequip_cmd().into_command())
            .expect("valid unequip should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "cosmetic.unequipped");
        match &events[0] {
            Event::CosmeticUnequipped(unequipped) => {
                assert_eq!(unequipped.player_id, "p-01");
                assert_eq!(unequipped.base_card_id, "c-01");
            }
            other => panic!("expected CosmeticUnequipped, got {other:?}"),
        }
        // The collection recorded the event.
        assert_eq!(collection.version(), 1);
        assert_eq!(collection.uncommitted_events().len(), 1);
        assert_eq!(
            collection.uncommitted_events()[0].event_type(),
            "cosmetic.unequipped"
        );
    }

    // Scenario: rejected — a card may only be included in an Outfit if it is
    // present (qty ≥ 1) in the collection.
    #[test]
    fn unequip_rejects_when_base_card_not_present() {
        let mut collection = ready_collection();
        // Owned, but at quantity zero — present-for-inclusion requires qty ≥ 1.
        collection.set_base_card_quantity(0);

        let err = collection
            .execute(valid_unequip_cmd().into_command())
            .expect_err("an at-quantity-zero base card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Scenario: rejected — owned card quantities are always non-negative.
    #[test]
    fn unequip_rejects_when_quantity_negative() {
        let mut collection = ready_collection();
        // A negative owned quantity is a corrupt state.
        collection.set_base_card_quantity(-1);

        let err = collection
            .execute(valid_unequip_cmd().into_command())
            .expect_err("a negative owned quantity must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Scenario: rejected — a cosmetic skin may only be equipped onto a base card
    // the player actually owns.
    #[test]
    fn unequip_rejects_when_base_card_not_owned() {
        let mut collection = ready_collection();
        // The base card the skin targets is not in the collection at all.
        collection.set_base_card_in_collection(false);

        let err = collection
            .execute(valid_unequip_cmd().into_command())
            .expect_err("unequipping from an unowned base card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Scenario: rejected — cosmetic equips are resolved server-side and never
    // trusted from the client.
    #[test]
    fn unequip_rejects_when_client_asserted() {
        let mut collection = ready_collection();
        // The unequip was asserted by the client, not resolved server-side.
        collection.set_server_resolved(false);

        let err = collection
            .execute(valid_unequip_cmd().into_command())
            .expect_err("a client-asserted unequip must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // A command naming a different player is rejected before any invariant runs.
    #[test]
    fn unequip_rejects_command_for_a_different_player() {
        let mut collection = ready_collection();
        let cmd = UnequipCosmetic::new("p-99", "c-01");

        let err = collection
            .execute(cmd.into_command())
            .expect_err("a command for another player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Commands missing any required field are rejected.
    #[test]
    fn unequip_rejects_command_with_missing_fields() {
        for cmd in [
            UnequipCosmetic::new("   ", "c-01"),
            UnequipCosmetic::new("p-01", "   "),
        ] {
            let mut collection = ready_collection();
            let err = collection
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(collection.version(), 0);
        }
    }

    #[test]
    fn unequip_command_payload_round_trips() {
        let cmd = valid_unequip_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, UnequipCosmetic::COMMAND);
        let decoded: UnequipCosmetic = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_unequip_cmd());
    }

    // ---- GrantCardsCmd (S-36) ---------------------------------------------

    /// A command granting two copies of `c-01` and one of `c-02` to `p-01` from
    /// a pack opening.
    fn valid_grant_cmd() -> GrantCards {
        GrantCards::new(
            "p-01",
            vec![CardGrant::new("c-01", 2), CardGrant::new("c-02", 1)],
            "pack",
        )
    }

    // Scenario: Successfully execute GrantCardsCmd.
    #[test]
    fn grants_and_emits_cards_granted_event() {
        let mut collection = ready_collection();

        let events = collection
            .execute(valid_grant_cmd().into_command())
            .expect("valid grant should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "cards.granted");
        match &events[0] {
            Event::CardsGranted(granted) => {
                assert_eq!(granted.player_id, "p-01");
                assert_eq!(granted.source, "pack");
                assert_eq!(
                    granted.card_grants,
                    vec![CardGrant::new("c-01", 2), CardGrant::new("c-02", 1)]
                );
            }
            other => panic!("expected CardsGranted, got {other:?}"),
        }
        // The collection recorded the event.
        assert_eq!(collection.version(), 1);
        assert_eq!(collection.uncommitted_events().len(), 1);
        assert_eq!(
            collection.uncommitted_events()[0].event_type(),
            "cards.granted"
        );
    }

    // Scenario: rejected — a card may only be included in an Outfit if it is
    // present (qty ≥ 1) in the collection.
    #[test]
    fn grant_rejects_when_base_card_not_present() {
        let mut collection = ready_collection();
        collection.set_base_card_quantity(0);

        let err = collection
            .execute(valid_grant_cmd().into_command())
            .expect_err("an at-quantity-zero base card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Scenario: rejected — owned card quantities are always non-negative.
    #[test]
    fn grant_rejects_when_quantity_negative() {
        let mut collection = ready_collection();
        collection.set_base_card_quantity(-1);

        let err = collection
            .execute(valid_grant_cmd().into_command())
            .expect_err("a negative owned quantity must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Scenario: rejected — a cosmetic skin may only be equipped onto a base card
    // the player actually owns (the collection must own the referenced base card).
    #[test]
    fn grant_rejects_when_base_card_not_owned() {
        let mut collection = ready_collection();
        collection.set_base_card_in_collection(false);

        let err = collection
            .execute(valid_grant_cmd().into_command())
            .expect_err("granting against an unowned base card must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Scenario: rejected — cosmetic equips (card mutations) are resolved
    // server-side and never trusted from the client.
    #[test]
    fn grant_rejects_when_client_asserted() {
        let mut collection = ready_collection();
        collection.set_server_resolved(false);

        let err = collection
            .execute(valid_grant_cmd().into_command())
            .expect_err("a client-asserted grant must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // A grant carrying a negative per-grant quantity is rejected up front.
    #[test]
    fn grant_rejects_negative_grant_quantity() {
        let mut collection = ready_collection();
        let cmd = GrantCards::new("p-01", vec![CardGrant::new("c-01", -3)], "reward");

        let err = collection
            .execute(cmd.into_command())
            .expect_err("a negative granted quantity must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // A grant naming a different player is rejected before any invariant runs.
    #[test]
    fn grant_rejects_command_for_a_different_player() {
        let mut collection = ready_collection();
        let cmd = GrantCards::new("p-99", vec![CardGrant::new("c-01", 1)], "reward");

        let err = collection
            .execute(cmd.into_command())
            .expect_err("a grant for another player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(collection.version(), 0);
    }

    // Grants missing a required field (player, source, grants, or card id) are
    // rejected.
    #[test]
    fn grant_rejects_command_with_missing_fields() {
        let cmds = [
            GrantCards::new("   ", vec![CardGrant::new("c-01", 1)], "pack"),
            GrantCards::new("p-01", vec![CardGrant::new("c-01", 1)], "   "),
            GrantCards::new("p-01", vec![], "pack"),
            GrantCards::new("p-01", vec![CardGrant::new("   ", 1)], "pack"),
        ];
        for cmd in cmds {
            let mut collection = ready_collection();
            let err = collection
                .execute(cmd.into_command())
                .expect_err("a grant with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(collection.version(), 0);
        }
    }

    #[test]
    fn grant_command_payload_round_trips() {
        let cmd = valid_grant_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, GrantCards::COMMAND);
        let decoded: GrantCards = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_grant_cmd());
    }
}
