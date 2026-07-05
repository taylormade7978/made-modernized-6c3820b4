//! CardPack bounded context — an openable pack of cards in the
//! shop-and-payments context.
//!
//! A [`CardPack`] is a single purchasable pack whose contents are rolled from a
//! seeded RNG table and then *revealed* for granting. Three invariants govern
//! whether a pack's contents may be revealed, and every one of them is
//! re-checked when a reveal is requested:
//!
//! 1. **Seeded RNG / fixed rarity distribution** — pack contents are drawn from
//!    a seeded RNG table with a fixed rarity distribution; contents rolled
//!    outside that table (or against a tampered distribution) may not be
//!    revealed.
//! 2. **Opened exactly once** — a pack may be opened exactly once and its
//!    revealed contents are immutable; a pack already opened may not be revealed
//!    a second time.
//! 3. **Duplicate-protection / pity honored** — duplicate-protection and pity
//!    rules are honored across a player's openings; a pack whose roll violates
//!    those rules may not be revealed.
//!
//! The only command implemented so far is [`RevealPackContents`]
//! (`RevealPackContentsCmd`): it finalizes and surfaces the rolled contents for
//! granting, enforcing every invariant, and on success emits
//! [`Event::PackContentsRevealed`] (`pack.contents.revealed`). This module is
//! hand-written (it does not use `shared::stub_aggregate!`) but preserves the
//! same public surface — a [`CardPack`] aggregate and a [`CardPackRepository`]
//! port — so any persistence adapters compile against it unchanged, exactly like
//! its sibling [`Order`](crate::order).

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "CardPack";

/// The command name [`CardPack::execute`] recognizes.
const REVEAL_PACK_CONTENTS: &str = "RevealPackContentsCmd";

/// The `RevealPackContentsCmd` payload: which CardPack is being revealed. Field
/// names use the shop service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`RevealPackContents::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`CardPack::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealPackContents {
    /// The CardPack whose contents are revealed; must name this CardPack, and
    /// must be non-empty.
    pub pack_id: String,
}

impl RevealPackContents {
    /// The command name this maps to.
    pub const COMMAND: &'static str = REVEAL_PACK_CONTENTS;

    /// Build a command revealing the contents of `pack_id`.
    pub fn new(pack_id: impl Into<String>) -> Self {
        Self {
            pack_id: pack_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`CardPack::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("RevealPackContents is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The pack whose contents were revealed, carried by
/// [`Event::PackContentsRevealed`] and thus by the emitted
/// `pack.contents.revealed` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackContentsRevealed {
    /// The CardPack whose contents were revealed.
    pub pack_id: String,
}

/// Domain events emitted by [`CardPack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The pack's rolled contents were finalized and surfaced for granting.
    PackContentsRevealed(PackContentsRevealed),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::PackContentsRevealed(_) => "pack.contents.revealed",
        }
    }
}

/// The CardPack aggregate: one openable pack whose rolled contents are revealed
/// for granting.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring is unchanged, while it
/// now carries the state the [`RevealPackContents`] command validates against:
/// whether the contents were drawn from the seeded RNG table with its fixed
/// rarity distribution, whether the pack has already been opened, and whether
/// duplicate-protection / pity rules were honored for the roll.
///
/// A fresh CardPack from [`CardPack::new`] is revealable: its contents came from
/// the seeded RNG table, it has not been opened, and its roll honors
/// duplicate-protection / pity rules. The configuration methods below drive it
/// to a state a command rejects, exactly as [`Order`](crate::order) is built up
/// before a command validates it.
#[derive(Debug)]
pub struct CardPack {
    id: String,
    root: AggregateRoot,
    /// Whether the pack contents were drawn from the seeded RNG table with its
    /// fixed rarity distribution. `false` means the roll came from outside that
    /// table (or against a tampered distribution), which is never allowed.
    contents_from_seeded_rng: bool,
    /// Whether the pack has already been opened. A pack may be opened exactly
    /// once and its revealed contents are immutable, so revealing an
    /// already-opened pack is rejected.
    already_opened: bool,
    /// Whether the roll honors duplicate-protection / pity rules across the
    /// player's openings.
    duplicate_protection_honored: bool,
}

impl CardPack {
    /// Create a new, revealable CardPack identified by `id`: its contents came
    /// from the seeded RNG table with its fixed rarity distribution, it has not
    /// yet been opened, and its roll honors duplicate-protection / pity rules.
    /// Use the configuration methods to drive it to the state a command
    /// validates.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            contents_from_seeded_rng: true,
            already_opened: false,
            duplicate_protection_honored: true,
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

    /// Record whether the pack contents were drawn from the seeded RNG table
    /// with its fixed rarity distribution (`false` models a roll made outside
    /// that table).
    pub fn set_contents_from_seeded_rng(&mut self, ok: bool) {
        self.contents_from_seeded_rng = ok;
    }

    /// Record whether the pack has already been opened.
    pub fn set_already_opened(&mut self, opened: bool) {
        self.already_opened = opened;
    }

    /// Record whether the roll honors duplicate-protection / pity rules.
    pub fn set_duplicate_protection_honored(&mut self, ok: bool) {
        self.duplicate_protection_honored = ok;
    }

    /// RNG invariant: pack contents are drawn from a seeded RNG table with a
    /// fixed rarity distribution.
    fn ensure_contents_from_seeded_rng(&self) -> Result<(), DomainError> {
        if !self.contents_from_seeded_rng {
            return Err(DomainError::InvariantViolation(format!(
                "card pack '{}' contents were not drawn from the seeded RNG table; pack contents \
                 are drawn from a seeded RNG table with a fixed rarity distribution",
                self.id
            )));
        }
        Ok(())
    }

    /// Immutability invariant: a pack may be opened exactly once; revealed
    /// contents are immutable.
    fn ensure_not_already_opened(&self) -> Result<(), DomainError> {
        if self.already_opened {
            return Err(DomainError::InvariantViolation(format!(
                "card pack '{}' has already been opened; a pack may be opened exactly once and its \
                 revealed contents are immutable",
                self.id
            )));
        }
        Ok(())
    }

    /// Pity invariant: duplicate-protection / pity rules are honored across a
    /// player's openings.
    fn ensure_duplicate_protection_honored(&self) -> Result<(), DomainError> {
        if !self.duplicate_protection_honored {
            return Err(DomainError::InvariantViolation(format!(
                "card pack '{}' roll violates duplicate-protection / pity rules; \
                 duplicate-protection / pity rules are honored across a player's openings",
                self.id
            )));
        }
        Ok(())
    }

    /// Handle `RevealPackContentsCmd`: verify the command carries a valid packId
    /// (naming this CardPack), enforce every invariant (seeded RNG / fixed rarity
    /// distribution, opened-exactly-once, and duplicate-protection / pity), mark
    /// the pack opened, and emit [`Event::PackContentsRevealed`].
    fn reveal_pack_contents(&mut self, cmd: RevealPackContents) -> Result<Vec<Event>, DomainError> {
        // A valid packId must be supplied.
        if cmd.pack_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card pack '{}' requires a valid packId to reveal contents",
                self.id
            )));
        }
        // The command must name the CardPack it is dispatched to.
        if cmd.pack_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets card pack '{}' but this aggregate is card pack '{}'",
                cmd.pack_id, self.id
            )));
        }

        // Enforce every invariant before recording the reveal.
        self.ensure_contents_from_seeded_rng()?;
        self.ensure_not_already_opened()?;
        self.ensure_duplicate_protection_honored()?;

        // Mark the pack opened so a repeated reveal is rejected by the
        // opened-exactly-once invariant — revealed contents are immutable.
        self.already_opened = true;

        let event = Event::PackContentsRevealed(PackContentsRevealed {
            pack_id: cmd.pack_id,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for CardPack {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            REVEAL_PACK_CONTENTS => {
                let cmd: RevealPackContents =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed RevealPackContentsCmd payload: {e}"
                        ))
                    })?;
                self.reveal_pack_contents(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`CardPack`] aggregate. Adapters implement
/// [`shared::Repository`] for `CardPack` and then this marker trait.
pub trait CardPackRepository: Repository<CardPack> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A revealable CardPack `cp-01`: contents from the seeded RNG table, not yet
    /// opened, duplicate-protection / pity honored. Tests mutate one aspect at a
    /// time to drive a specific rejection.
    fn ready_pack() -> CardPack {
        let mut pack = CardPack::new("cp-01");
        pack.set_contents_from_seeded_rng(true);
        pack.set_already_opened(false);
        pack.set_duplicate_protection_honored(true);
        pack
    }

    /// A command revealing the contents of card pack `cp-01`.
    fn valid_cmd() -> RevealPackContents {
        RevealPackContents::new("cp-01")
    }

    // Scenario: Successfully execute RevealPackContentsCmd.
    #[test]
    fn reveals_and_emits_pack_contents_revealed_event() {
        let mut pack = ready_pack();

        let events = pack
            .execute(valid_cmd().into_command())
            .expect("valid reveal should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "pack.contents.revealed");
        match &events[0] {
            Event::PackContentsRevealed(revealed) => {
                assert_eq!(revealed.pack_id, "cp-01");
            }
        }
        // The CardPack recorded the event.
        assert_eq!(pack.version(), 1);
        assert_eq!(pack.uncommitted_events().len(), 1);
        assert_eq!(
            pack.uncommitted_events()[0].event_type(),
            "pack.contents.revealed"
        );
    }

    // Scenario: rejected — Pack contents are drawn from a seeded RNG table with a
    // fixed rarity distribution.
    #[test]
    fn rejects_when_contents_not_from_seeded_rng() {
        let mut pack = ready_pack();
        // The roll came from outside the seeded RNG table / a tampered distribution.
        pack.set_contents_from_seeded_rng(false);

        let err = pack
            .execute(valid_cmd().into_command())
            .expect_err("a pack rolled outside the seeded RNG table must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pack.version(), 0);
    }

    // Scenario: rejected — A pack may be opened exactly once; revealed contents
    // are immutable.
    #[test]
    fn rejects_when_already_opened() {
        let mut pack = ready_pack();
        // The pack has already been opened once.
        pack.set_already_opened(true);

        let err = pack
            .execute(valid_cmd().into_command())
            .expect_err("an already-opened pack must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pack.version(), 0);
    }

    // Opened-exactly-once in practice: a second reveal of the same pack is
    // rejected because the first marked it opened (revealed contents immutable).
    #[test]
    fn rejects_a_repeated_reveal_of_the_same_pack() {
        let mut pack = ready_pack();

        pack.execute(valid_cmd().into_command())
            .expect("first reveal should succeed");
        // The pack is asked to reveal its contents again.
        let err = pack
            .execute(valid_cmd().into_command())
            .expect_err("a repeated reveal must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        // Still exactly one recorded event — contents are immutable.
        assert_eq!(pack.version(), 1);
        assert_eq!(pack.uncommitted_events().len(), 1);
    }

    // Scenario: rejected — Duplicate-protection / pity rules are honored across a
    // player's openings.
    #[test]
    fn rejects_when_duplicate_protection_violated() {
        let mut pack = ready_pack();
        // The roll violates duplicate-protection / pity rules.
        pack.set_duplicate_protection_honored(false);

        let err = pack
            .execute(valid_cmd().into_command())
            .expect_err("a roll violating duplicate-protection / pity rules must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pack.version(), 0);
    }

    // A command naming a different CardPack is rejected before any invariant runs.
    #[test]
    fn rejects_command_for_a_different_pack() {
        let mut pack = ready_pack();
        let cmd = RevealPackContents::new("cp-99");

        let err = pack
            .execute(cmd.into_command())
            .expect_err("a command for another pack must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pack.version(), 0);
    }

    // Commands missing the required packId are rejected.
    #[test]
    fn rejects_command_with_missing_pack_id() {
        let mut pack = ready_pack();
        let err = pack
            .execute(RevealPackContents::new("   ").into_command())
            .expect_err("a command with a missing packId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pack.version(), 0);
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut pack = CardPack::new("cp-01");
        let err = pack.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "CardPack");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, RevealPackContents::COMMAND);
        let decoded: RevealPackContents = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
