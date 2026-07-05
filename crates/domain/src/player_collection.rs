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
//! The only command implemented so far is [`EquipCosmetic`]
//! (`EquipCosmeticCmd`): it equips a cosmetic skin onto an owned base card,
//! enforcing every invariant, and on success emits [`Event::CosmeticEquipped`]
//! (`cosmetic.equipped`). This module is hand-written (it does not use
//! `shared::stub_aggregate!`) but preserves the same public surface — a
//! [`PlayerCollection`] aggregate and a [`PlayerCollectionRepository`] port — so
//! the persistence adapters in `crates/mocks` compile against it unchanged.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "PlayerCollection";

/// The command name [`PlayerCollection::execute`] recognizes.
const EQUIP_COSMETIC: &str = "EquipCosmeticCmd";

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

/// Domain events emitted by [`PlayerCollection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A cosmetic skin was equipped onto an owned base card.
    CosmeticEquipped(CosmeticEquipped),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::CosmeticEquipped(_) => "cosmetic.equipped",
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
}
