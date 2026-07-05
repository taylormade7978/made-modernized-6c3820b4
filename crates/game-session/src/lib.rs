//! GameSession bounded context — the authoritative card-game rules aggregate.
//!
//! This crate is special among the bounded contexts: it compiles both natively
//! (linked by the actix-web server for authoritative server-side execution) and
//! to `wasm32` via `wasm-pack`, so the browser client can run the *same* rules
//! for prediction/validation. To keep that dual target working, the crate
//! depends only on the WASM-safe [`shared`] kernel plus `serde`/`serde_json`
//! (both compile to `wasm32`); the browser entrypoint is isolated behind the
//! optional `wasm` feature.
//!
//! Build for the web:
//! `wasm-pack build crates/game-session -- --features wasm`
//!
//! The first command implemented is [`StartMatch`] (`StartMatchCmd`): it
//! initializes a session from two [`OutfitConfig`]s, an RNG seed, and each
//! Outfit's Boss, enforcing the match-play rules-contract invariants up front,
//! and on success emits [`Event::MatchStarted`] (`match.started`).
//!
//! [`Mulligan`] (`MulliganCmd`) then applies a player's opening-hand redraw
//! selection: it validates the redraw request for the turn-holding player,
//! re-checks the same rules-contract invariants against the session's opening
//! state, and on success emits [`Event::MulliganCompleted`]
//! (`mulligan.completed`).
//!
//! [`PlayCard`] (`PlayCardCmd`) then plays a card from the turn-holding player's
//! hand at a target: it pays the card's Juice cost (rejecting a card the player
//! cannot afford), re-checks the same rules-contract invariants, and raises the
//! player's Heat — so on success it emits *two* events, [`Event::CardPlayed`]
//! (`card.played`) followed by [`Event::HeatRaised`] (`heat.raised`). The module
//! is hand-written (it no longer uses `shared::stub_aggregate!`) but preserves
//! the same public surface — a [`GameSession`] aggregate and a
//! [`GameSessionRepository`] port — so the persistence adapters in
//! `crates/mocks` and the actix-web server keep compiling unchanged.
//!
//! [`DeclareAttack`] (`DeclareAttackCmd`) then declares the turn-holding
//! player's attacker into a defender, resolves combat simultaneously, and emits
//! [`Event::CombatResolved`] (`combat.resolved`) followed by
//! [`Event::BossDefeated`] (`boss.defeated`) when that combat drops the defending
//! Boss.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "GameSession";

/// The `StartMatchCmd` command name [`GameSession::execute`] recognizes.
const START_MATCH: &str = "StartMatchCmd";

/// The `MulliganCmd` command name [`GameSession::execute`] recognizes.
const MULLIGAN: &str = "MulliganCmd";

/// The `PlayCardCmd` command name [`GameSession::execute`] recognizes.
const PLAY_CARD: &str = "PlayCardCmd";

/// The `DeclareAttackCmd` command name [`GameSession::execute`] recognizes.
const DECLARE_ATTACK: &str = "DeclareAttackCmd";

/// Heat a player gains each time they play a card. Playing a card always raises
/// Heat, so a successful [`PlayCard`] emits an accompanying `heat.raised` event.
pub const HEAT_PER_PLAY: i32 = 1;

/// A player's board may hold at most this many Operators simultaneously.
pub const MAX_OPERATORS: usize = 7;

/// A player's board may hold at most this many Vehicles simultaneously.
pub const MAX_VEHICLES: usize = 3;

/// Juice a player starts a match with (it ramps +1 each of the owner's turns).
pub const STARTING_JUICE: u8 = 1;

/// Juice is hard-capped at this value; no state may exceed it.
pub const JUICE_CAP: u8 = 10;

/// Heat is bounded to this inclusive range; no state may leave it. Reaching the
/// upper bound immediately triggers a Cop Event, so a *clean* match start must
/// sit strictly below it (see [`GameSession::ensure_heat_within_bounds`]).
pub const HEAT_BOUNDS: RangeInclusive<i32> = 0..=10;

/// Which of the two players a value refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Player {
    /// The first Outfit (`playerAOutfit`).
    A,
    /// The second Outfit (`playerBOutfit`).
    B,
}

/// The starting configuration of one player's Outfit: the board it brings, its
/// deck, its Boss, and its opening resource counters. A match start validates
/// each Outfit against the rules-contract before the session is created.
///
/// [`OutfitConfig::new`] returns a *legal* opening Outfit; tests (and callers)
/// mutate one field at a time to model a specific rule violation, mirroring how
/// the other catalog aggregates are configured before a command validates them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutfitConfig {
    /// The Outfit's identifier; a [`StartMatch`] command names it explicitly.
    pub name: String,
    /// Operators this Outfit opens with on its board (cap [`MAX_OPERATORS`]).
    pub operators: usize,
    /// Vehicles this Outfit opens with on its board (cap [`MAX_VEHICLES`]).
    pub vehicles: usize,
    /// Cards in this Outfit's opening deck. Must be non-empty — drawing from an
    /// empty deck deals escalating Fatigue instead of yielding a card, which is
    /// not a legal way to *begin* a match.
    pub deck_size: usize,
    /// The Boss this Outfit fields.
    pub boss_name: String,
    /// The Boss's opening HP. Must be `> 0`; a match ends the instant a Boss's
    /// HP reaches 0 or below, so a Boss cannot start a match already defeated.
    pub boss_hp: i32,
    /// Opening Heat for this Outfit. Must sit within [`HEAT_BOUNDS`] and below
    /// the Cop-Event threshold at match start.
    pub starting_heat: i32,
    /// Opening Juice for this Outfit. Must equal [`STARTING_JUICE`].
    pub starting_juice: u8,
    /// The seat's *currently available* Juice pool — it ramps +1 from
    /// [`STARTING_JUICE`] each of the owner's turns and is hard-capped at
    /// [`JUICE_CAP`]. A card may only be played when its Juice cost does not
    /// exceed this amount (see [`GameSession::ensure_card_affordable`]).
    pub available_juice: u8,
    /// Whether a Heist has been marked resolved for this Outfit at start.
    pub heist_resolved: bool,
    /// Outstanding prerequisites in this Outfit's Heist prerequisite queue. A
    /// Heist may only resolve once this reaches zero.
    pub outstanding_heist_prereqs: usize,
}

impl OutfitConfig {
    /// A legal opening Outfit named `name`: a small board within caps, a
    /// non-empty deck, a healthy Boss, and the rules-contract opening counters
    /// (Heat 0, Juice [`STARTING_JUICE`], no unresolved Heist).
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            boss_name: format!("{name}-boss"),
            name,
            operators: 2,
            vehicles: 1,
            deck_size: 30,
            boss_hp: 30,
            starting_heat: 0,
            starting_juice: STARTING_JUICE,
            // A few turns in from the opening: enough ramped Juice to afford a
            // modestly-costed card, still comfortably within the hard cap.
            available_juice: 3,
            heist_resolved: false,
            outstanding_heist_prereqs: 0,
        }
    }
}

/// The `StartMatchCmd` payload: the match to open, the two competing Outfits by
/// name, and the RNG seed that makes the match deterministic. Field names are
/// the match-play schema's `camelCase`.
///
/// Build one directly and turn it into a [`Command`] with
/// [`StartMatch::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`GameSession::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartMatch {
    /// Identifier of the match being started; must name the match this session
    /// records.
    pub match_id: String,
    /// Name of the Outfit taking the `A` seat; must match the configured Outfit.
    pub player_a_outfit: String,
    /// Name of the Outfit taking the `B` seat; must match the configured Outfit.
    pub player_b_outfit: String,
    /// Deterministic RNG seed for the match. Must be non-zero to be a valid,
    /// reproducible seed.
    pub rng_seed: u64,
}

impl StartMatch {
    /// The command name this maps to.
    pub const COMMAND: &'static str = START_MATCH;

    /// Build a `StartMatchCmd` for `match_id` between `player_a`/`player_b` with
    /// RNG `seed`.
    pub fn new(
        match_id: impl Into<String>,
        player_a: impl Into<String>,
        player_b: impl Into<String>,
        seed: u64,
    ) -> Self {
        Self {
            match_id: match_id.into(),
            player_a_outfit: player_a.into(),
            player_b_outfit: player_b.into(),
            rng_seed: seed,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`GameSession::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("StartMatch is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `MulliganCmd` payload: the match being played, the player performing the
/// opening-hand redraw, and the specific card identities that player is sending
/// back to be redrawn. Field names are the match-play schema's `camelCase`.
///
/// Build one directly and turn it into a [`Command`] with
/// [`Mulligan::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`GameSession::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mulligan {
    /// Identifier of the match being played; must name the match this session
    /// records.
    pub match_id: String,
    /// Identity of the player performing the redraw; must name one of this
    /// session's configured Outfits, and it must be that player's turn.
    pub player_id: String,
    /// The card identities the player is redrawing. May be empty (the player
    /// keeps their whole hand); every id present must be non-blank and distinct,
    /// and there cannot be more of them than the player's deck can replace.
    pub card_ids_to_redraw: Vec<String>,
}

impl Mulligan {
    /// The command name this maps to.
    pub const COMMAND: &'static str = MULLIGAN;

    /// Build a `MulliganCmd` for `player_id` in `match_id`, redrawing
    /// `card_ids_to_redraw`.
    pub fn new(
        match_id: impl Into<String>,
        player_id: impl Into<String>,
        card_ids_to_redraw: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            match_id: match_id.into(),
            player_id: player_id.into(),
            card_ids_to_redraw: card_ids_to_redraw.into_iter().map(Into::into).collect(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`GameSession::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("Mulligan is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `PlayCardCmd` payload: the match being played, the player playing the
/// card, the specific card instance leaving their hand, the target the card
/// resolves against, and the card's Juice cost. Field names are the match-play
/// schema's `camelCase`.
///
/// Build one directly and turn it into a [`Command`] with
/// [`PlayCard::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`GameSession::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayCard {
    /// Identifier of the match being played; must name the match this session
    /// records.
    pub match_id: String,
    /// Identity of the player playing the card; must name one of this session's
    /// configured Outfits, and it must be that player's turn.
    pub player_id: String,
    /// The specific card instance being played out of the player's hand. Must be
    /// non-blank.
    pub card_instance_id: String,
    /// A reference to the target the card resolves against. Must be non-blank.
    pub target_ref: String,
    /// The card's Juice cost. A card may only be played when its cost does not
    /// exceed the player's currently available Juice.
    pub juice_cost: u8,
}

impl PlayCard {
    /// The command name this maps to.
    pub const COMMAND: &'static str = PLAY_CARD;

    /// Build a `PlayCardCmd` playing `card_instance_id` at `target_ref` for
    /// `player_id` in `match_id`, paying `juice_cost` Juice.
    pub fn new(
        match_id: impl Into<String>,
        player_id: impl Into<String>,
        card_instance_id: impl Into<String>,
        target_ref: impl Into<String>,
        juice_cost: u8,
    ) -> Self {
        Self {
            match_id: match_id.into(),
            player_id: player_id.into(),
            card_instance_id: card_instance_id.into(),
            target_ref: target_ref.into(),
            juice_cost,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`GameSession::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("PlayCard is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `DeclareAttackCmd` payload: the match being played, the player declaring
/// the attack, the attacker they are committing, and the defender being attacked.
/// Field names are the match-play schema's `camelCase`.
///
/// Build one directly and turn it into a [`Command`] with
/// [`DeclareAttack::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`GameSession::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclareAttack {
    /// Identifier of the match being played; must name the match this session
    /// records.
    pub match_id: String,
    /// Identity of the player declaring the attack; must name one of this
    /// session's configured Outfits, and it must be that player's turn.
    pub player_id: String,
    /// The attacking combatant. Must be non-blank.
    pub attacker_id: String,
    /// The defending target. Must be non-blank; in this slice it is treated as
    /// the opposing Boss target that is defeated by the resolved combat.
    pub defender_id: String,
}

impl DeclareAttack {
    /// The command name this maps to.
    pub const COMMAND: &'static str = DECLARE_ATTACK;

    /// Build a `DeclareAttackCmd` for `player_id` in `match_id`, committing
    /// `attacker_id` against `defender_id`.
    pub fn new(
        match_id: impl Into<String>,
        player_id: impl Into<String>,
        attacker_id: impl Into<String>,
        defender_id: impl Into<String>,
    ) -> Self {
        Self {
            match_id: match_id.into(),
            player_id: player_id.into(),
            attacker_id: attacker_id.into(),
            defender_id: defender_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`GameSession::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("DeclareAttack is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The started match, carried by [`Event::MatchStarted`] and thus by the emitted
/// `match.started` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchStarted {
    /// The match that was opened.
    pub match_id: String,
    /// The Outfit seated as player `A`.
    pub player_a_outfit: String,
    /// The Outfit seated as player `B`.
    pub player_b_outfit: String,
    /// The RNG seed the match was opened with.
    pub rng_seed: u64,
    /// The player whose turn opens the match.
    pub opening_player: Player,
}

/// A completed opening-hand redraw, carried by [`Event::MulliganCompleted`] and
/// thus by the emitted `mulligan.completed` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MulliganCompleted {
    /// The match the redraw happened in.
    pub match_id: String,
    /// The player identity that redrew.
    pub player_id: String,
    /// The seat that player occupies.
    pub player: Player,
    /// The card identities that were redrawn (in the order submitted).
    pub redrawn_card_ids: Vec<String>,
}

/// A played card, carried by [`Event::CardPlayed`] and thus by the emitted
/// `card.played` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardPlayed {
    /// The match the card was played in.
    pub match_id: String,
    /// The player identity that played the card.
    pub player_id: String,
    /// The seat that player occupies.
    pub player: Player,
    /// The card instance that left the player's hand.
    pub card_instance_id: String,
    /// The target the card resolved against.
    pub target_ref: String,
    /// The Juice paid to play the card.
    pub juice_spent: u8,
}

/// A Heat increase, carried by [`Event::HeatRaised`] and thus by the emitted
/// `heat.raised` event. Playing a card always raises the player's Heat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeatRaised {
    /// The match the Heat was raised in.
    pub match_id: String,
    /// The seat whose Heat rose.
    pub player: Player,
    /// How much Heat was gained.
    pub amount: i32,
    /// The player's resulting Heat after the raise (always within [`HEAT_BOUNDS`]).
    pub new_heat: i32,
}

/// A completed simultaneous combat resolution, carried by
/// [`Event::CombatResolved`] and thus by the emitted `combat.resolved` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombatResolved {
    /// The match the combat happened in.
    pub match_id: String,
    /// The player identity that declared the attack.
    pub attacking_player_id: String,
    /// The seat that declared the attack.
    pub attacking_player: Player,
    /// The attacking combatant.
    pub attacker_id: String,
    /// The seat defending against the attack.
    pub defending_player: Player,
    /// The defending target.
    pub defender_id: String,
}

/// A Boss defeated by resolved combat, carried by [`Event::BossDefeated`] and
/// thus by the emitted `boss.defeated` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BossDefeated {
    /// The match the defeat happened in.
    pub match_id: String,
    /// The player identity whose Boss was defeated.
    pub defeated_player_id: String,
    /// The seat whose Boss was defeated.
    pub defeated_player: Player,
    /// The Boss target defeated by combat.
    pub boss_id: String,
    /// The winning seat. This command resolves a single attacker into the
    /// opposing Boss, so exactly one winner is produced.
    pub winner: Player,
}

/// Domain events emitted by [`GameSession`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A match passed every start-time invariant and was initialized.
    MatchStarted(MatchStarted),
    /// A player's opening-hand redraw passed every invariant and was applied.
    MulliganCompleted(MulliganCompleted),
    /// A card passed every invariant, was paid for, and was played.
    CardPlayed(CardPlayed),
    /// Playing a card raised the acting player's Heat.
    HeatRaised(HeatRaised),
    /// A declared attack was resolved simultaneously.
    CombatResolved(CombatResolved),
    /// Resolved combat defeated a Boss and ended the match for one winner.
    BossDefeated(BossDefeated),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::MatchStarted(_) => "match.started",
            Event::MulliganCompleted(_) => "mulligan.completed",
            Event::CardPlayed(_) => "card.played",
            Event::HeatRaised(_) => "heat.raised",
            Event::CombatResolved(_) => "combat.resolved",
            Event::BossDefeated(_) => "boss.defeated",
        }
    }
}

/// The GameSession aggregate: the authoritative state of a single match.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the actix-web server — is unchanged, while it now
/// carries the match's opening setup: the match it records, both players'
/// [`OutfitConfig`]s, and which player is designated to move first.
///
/// A fresh session from [`GameSession::new`] is intentionally set up as a *legal*
/// opening (two default Outfits, player `A` to move); callers reshape it via the
/// configuration methods, exactly as [`ExpansionSet`](../expansion_set) is built
/// up before a command validates it. `execute` handles [`StartMatchCmd`].
#[derive(Debug)]
pub struct GameSession {
    id: String,
    root: AggregateRoot,
    /// The match this session records; a [`StartMatch`] command must name it.
    match_id: String,
    /// Opening configuration for the player-`A` Outfit.
    player_a: OutfitConfig,
    /// Opening configuration for the player-`B` Outfit.
    player_b: OutfitConfig,
    /// The player designated to take the first turn. `None` models an
    /// ill-formed setup with no whose-turn-it-is — an invalid start, since a
    /// command is only valid for the player whose turn it currently is.
    opening_player: Option<Player>,
}

impl GameSession {
    /// Create a new session identified by `id`, seeded with a legal opening
    /// (two default Outfits named `<id>-a` / `<id>-b`, player `A` to move). Use
    /// the configuration methods to reshape it before starting the match.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        let player_a = OutfitConfig::new(format!("{id}-a"));
        let player_b = OutfitConfig::new(format!("{id}-b"));
        Self {
            match_id: id.clone(),
            id,
            root: AggregateRoot::new(),
            player_a,
            player_b,
            opening_player: Some(Player::A),
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The match this session records.
    pub fn match_id(&self) -> &str {
        &self.match_id
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// Set the opening configuration for the player-`A` Outfit.
    pub fn configure_player_a(&mut self, outfit: OutfitConfig) {
        self.player_a = outfit;
    }

    /// Set the opening configuration for the player-`B` Outfit.
    pub fn configure_player_b(&mut self, outfit: OutfitConfig) {
        self.player_b = outfit;
    }

    /// Designate which player takes the first turn (or `None` to model an
    /// ill-formed, turn-less setup).
    pub fn set_opening_player(&mut self, player: Option<Player>) {
        self.opening_player = player;
    }

    /// Both Outfits paired with the seat they occupy, for the per-Outfit
    /// invariant checks.
    fn outfits(&self) -> [(&OutfitConfig, Player); 2] {
        [(&self.player_a, Player::A), (&self.player_b, Player::B)]
    }

    /// Board invariant: a player's board may hold at most [`MAX_OPERATORS`]
    /// Operators and at most [`MAX_VEHICLES`] Vehicles simultaneously.
    fn ensure_boards_within_caps(&self) -> Result<(), DomainError> {
        for (outfit, seat) in self.outfits() {
            if outfit.operators > MAX_OPERATORS {
                return Err(DomainError::InvariantViolation(format!(
                    "player {seat:?} Outfit '{}' opens with {} Operators, exceeding the cap of {MAX_OPERATORS}",
                    outfit.name, outfit.operators
                )));
            }
            if outfit.vehicles > MAX_VEHICLES {
                return Err(DomainError::InvariantViolation(format!(
                    "player {seat:?} Outfit '{}' opens with {} Vehicles, exceeding the cap of {MAX_VEHICLES}",
                    outfit.name, outfit.vehicles
                )));
            }
        }
        Ok(())
    }

    /// Heat invariant: Heat is bounded to [`HEAT_BOUNDS`], and a clean start must
    /// sit strictly below the upper bound — reaching it immediately triggers a
    /// Cop Event, which cannot happen while a match is being initialized.
    fn ensure_heat_within_bounds(&self) -> Result<(), DomainError> {
        let cop_event_threshold = *HEAT_BOUNDS.end();
        for (outfit, seat) in self.outfits() {
            if !HEAT_BOUNDS.contains(&outfit.starting_heat)
                || outfit.starting_heat >= cop_event_threshold
            {
                return Err(DomainError::InvariantViolation(format!(
                    "player {seat:?} Outfit '{}' opens with Heat {}, which must be within [{}, {}) at match start (reaching {} triggers a Cop Event)",
                    outfit.name,
                    outfit.starting_heat,
                    HEAT_BOUNDS.start(),
                    cop_event_threshold,
                    cop_event_threshold
                )));
            }
        }
        Ok(())
    }

    /// Juice invariant: Juice starts at [`STARTING_JUICE`] and is hard-capped at
    /// [`JUICE_CAP`]; an opening Outfit must carry exactly the starting value.
    fn ensure_starting_juice_valid(&self) -> Result<(), DomainError> {
        for (outfit, seat) in self.outfits() {
            if outfit.starting_juice != STARTING_JUICE || outfit.starting_juice > JUICE_CAP {
                return Err(DomainError::InvariantViolation(format!(
                    "player {seat:?} Outfit '{}' opens with Juice {}, but Juice must start at {STARTING_JUICE} (hard-capped at {JUICE_CAP})",
                    outfit.name, outfit.starting_juice
                )));
            }
            if outfit.available_juice > JUICE_CAP {
                return Err(DomainError::InvariantViolation(format!(
                    "player {seat:?} Outfit '{}' has available Juice {}, exceeding the hard cap of {JUICE_CAP}",
                    outfit.name, outfit.available_juice
                )));
            }
        }
        Ok(())
    }

    /// Deck invariant: an Outfit must open with a non-empty deck. Drawing from an
    /// empty deck deals escalating Fatigue to the drawing Boss instead of
    /// yielding a card, so a match cannot legally *begin* deckless.
    fn ensure_decks_nonempty(&self) -> Result<(), DomainError> {
        for (outfit, seat) in self.outfits() {
            if outfit.deck_size == 0 {
                return Err(DomainError::InvariantViolation(format!(
                    "player {seat:?} Outfit '{}' opens with an empty deck; drawing would deal Fatigue instead of a card",
                    outfit.name
                )));
            }
        }
        Ok(())
    }

    /// Heist invariant: a Heist resolves only after every prerequisite in its
    /// Heist prerequisite queue has been satisfied — a start may not carry a
    /// Heist marked resolved while prerequisites remain outstanding.
    fn ensure_heists_prereqs_satisfied(&self) -> Result<(), DomainError> {
        for (outfit, seat) in self.outfits() {
            if outfit.heist_resolved && outfit.outstanding_heist_prereqs > 0 {
                return Err(DomainError::InvariantViolation(format!(
                    "player {seat:?} Outfit '{}' has a resolved Heist with {} outstanding prerequisite(s); a Heist resolves only once its queue is empty",
                    outfit.name, outfit.outstanding_heist_prereqs
                )));
            }
        }
        Ok(())
    }

    /// Match-end invariant: a match ends the instant either Boss's HP reaches 0
    /// or below, so both Bosses must open with HP `> 0`.
    fn ensure_bosses_alive(&self) -> Result<(), DomainError> {
        for (outfit, seat) in self.outfits() {
            if outfit.boss_hp <= 0 {
                return Err(DomainError::InvariantViolation(format!(
                    "player {seat:?} Boss '{}' opens with HP {}; a Boss at 0 or below ends the match instantly and cannot start one",
                    outfit.boss_name, outfit.boss_hp
                )));
            }
        }
        Ok(())
    }

    /// Turn-ownership invariant: a command is valid only for the player whose
    /// turn it currently is, so a started match must designate an opening player.
    fn ensure_opening_player_designated(&self) -> Result<Player, DomainError> {
        self.opening_player.ok_or_else(|| {
            DomainError::InvariantViolation(
                "match start designates no opening player; a command is only valid for the player whose turn it currently is".to_string(),
            )
        })
    }

    /// Handle `StartMatchCmd`: verify the command targets this match and names
    /// both Outfits and a valid seed, enforce every match-play start invariant,
    /// and emit [`Event::MatchStarted`].
    fn start_match(&mut self, cmd: StartMatch) -> Result<Vec<Event>, DomainError> {
        // The command must name the match this session actually records.
        if cmd.match_id != self.match_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets match '{}' but this session records '{}'",
                cmd.match_id, self.match_id
            )));
        }
        // Both seats must be named, distinct, and match the configured Outfits.
        if cmd.player_a_outfit.trim().is_empty() || cmd.player_b_outfit.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "both playerAOutfit and playerBOutfit must be provided".to_string(),
            ));
        }
        if cmd.player_a_outfit == cmd.player_b_outfit {
            return Err(DomainError::InvariantViolation(format!(
                "both seats name the same Outfit '{}'; a match needs two distinct Outfits",
                cmd.player_a_outfit
            )));
        }
        if cmd.player_a_outfit != self.player_a.name || cmd.player_b_outfit != self.player_b.name {
            return Err(DomainError::InvariantViolation(format!(
                "command names Outfits ('{}', '{}') that do not match this session's configured Outfits ('{}', '{}')",
                cmd.player_a_outfit, cmd.player_b_outfit, self.player_a.name, self.player_b.name
            )));
        }
        // A reproducible match needs a real seed.
        if cmd.rng_seed == 0 {
            return Err(DomainError::InvariantViolation(
                "a valid, non-zero rngSeed must be provided".to_string(),
            ));
        }

        // Enforce every match-play start invariant before initializing anything.
        self.ensure_boards_within_caps()?;
        self.ensure_heat_within_bounds()?;
        self.ensure_starting_juice_valid()?;
        self.ensure_decks_nonempty()?;
        self.ensure_heists_prereqs_satisfied()?;
        self.ensure_bosses_alive()?;
        let opening_player = self.ensure_opening_player_designated()?;

        let event = Event::MatchStarted(MatchStarted {
            match_id: cmd.match_id,
            player_a_outfit: cmd.player_a_outfit,
            player_b_outfit: cmd.player_b_outfit,
            rng_seed: cmd.rng_seed,
            opening_player,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Resolve a player identity to the seat it occupies, rejecting an identity
    /// that names neither configured Outfit.
    fn seat_for_player(&self, player_id: &str) -> Result<Player, DomainError> {
        if player_id == self.player_a.name {
            Ok(Player::A)
        } else if player_id == self.player_b.name {
            Ok(Player::B)
        } else {
            Err(DomainError::InvariantViolation(format!(
                "player '{player_id}' names neither configured Outfit ('{}', '{}')",
                self.player_a.name, self.player_b.name
            )))
        }
    }

    /// The opening Outfit seated at `seat`.
    fn outfit_at(&self, seat: Player) -> &OutfitConfig {
        match seat {
            Player::A => &self.player_a,
            Player::B => &self.player_b,
        }
    }

    /// Mutable access to the opening Outfit seated at `seat`.
    fn outfit_at_mut(&mut self, seat: Player) -> &mut OutfitConfig {
        match seat {
            Player::A => &mut self.player_a,
            Player::B => &mut self.player_b,
        }
    }

    /// The opposing seat for combat targeting.
    fn opponent_of(seat: Player) -> Player {
        match seat {
            Player::A => Player::B,
            Player::B => Player::A,
        }
    }

    /// Redraw-selection invariant: every card id must be non-blank and distinct,
    /// and a player cannot redraw more cards than their deck can replace — drawing
    /// past the deck deals Fatigue instead of yielding a card.
    fn ensure_redraw_selection_valid(
        &self,
        seat: Player,
        card_ids: &[String],
    ) -> Result<(), DomainError> {
        for id in card_ids {
            if id.trim().is_empty() {
                return Err(DomainError::InvariantViolation(
                    "cardIdsToRedraw contains a blank card id".to_string(),
                ));
            }
        }
        let mut seen = card_ids.to_vec();
        seen.sort();
        seen.dedup();
        if seen.len() != card_ids.len() {
            return Err(DomainError::InvariantViolation(
                "cardIdsToRedraw contains duplicate card ids; each redrawn card must be distinct"
                    .to_string(),
            ));
        }
        let deck_size = self.outfit_at(seat).deck_size;
        if card_ids.len() > deck_size {
            return Err(DomainError::InvariantViolation(format!(
                "redraw of {} card(s) exceeds the {} card(s) available in the deck; drawing past an empty deck deals Fatigue instead of a card",
                card_ids.len(),
                deck_size
            )));
        }
        Ok(())
    }

    /// Handle `MulliganCmd`: verify the command targets this match and a real
    /// player, validate the redraw selection, enforce every match-play invariant
    /// against the session's opening state, confirm it is the redrawing player's
    /// turn, and emit [`Event::MulliganCompleted`].
    fn mulligan(&mut self, cmd: Mulligan) -> Result<Vec<Event>, DomainError> {
        // The command must name the match this session actually records.
        if cmd.match_id != self.match_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets match '{}' but this session records '{}'",
                cmd.match_id, self.match_id
            )));
        }
        // A player must be named, and it must be one of the configured Outfits.
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "a playerId must be provided".to_string(),
            ));
        }
        let seat = self.seat_for_player(&cmd.player_id)?;

        // The redraw selection itself must be well-formed.
        self.ensure_redraw_selection_valid(seat, &cmd.card_ids_to_redraw)?;

        // Enforce every match-play invariant before applying the redraw.
        self.ensure_boards_within_caps()?;
        self.ensure_heat_within_bounds()?;
        self.ensure_starting_juice_valid()?;
        self.ensure_decks_nonempty()?;
        self.ensure_heists_prereqs_satisfied()?;
        self.ensure_bosses_alive()?;
        let turn_player = self.ensure_opening_player_designated()?;

        // Turn-ownership: a mulligan is valid only for the player whose turn it is.
        if seat != turn_player {
            return Err(DomainError::InvariantViolation(format!(
                "player '{}' (seat {seat:?}) may not mulligan; it is player {turn_player:?}'s turn",
                cmd.player_id
            )));
        }

        let event = Event::MulliganCompleted(MulliganCompleted {
            match_id: cmd.match_id,
            player_id: cmd.player_id,
            player: seat,
            redrawn_card_ids: cmd.card_ids_to_redraw,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Juice-affordability invariant: Juice starts at [`STARTING_JUICE`], ramps
    /// +1 each of the owner's turns, and is hard-capped at [`JUICE_CAP`]; a card
    /// may only be played when its Juice cost does not exceed the seat's
    /// currently available Juice.
    fn ensure_card_affordable(&self, seat: Player, juice_cost: u8) -> Result<(), DomainError> {
        let available = self.outfit_at(seat).available_juice;
        if available > JUICE_CAP {
            return Err(DomainError::InvariantViolation(format!(
                "player {seat:?} has available Juice {available}, exceeding the hard cap of {JUICE_CAP}"
            )));
        }
        if juice_cost > available {
            return Err(DomainError::InvariantViolation(format!(
                "card costs {juice_cost} Juice but player {seat:?} only has {available} available; a card may only be played when its cost does not exceed available Juice"
            )));
        }
        Ok(())
    }

    /// The Heat the acting seat would hold after playing a card, guaranteeing the
    /// raise keeps Heat within [`HEAT_BOUNDS`] — no state may leave Heat outside
    /// `[0, 10]`.
    fn heat_after_play(&self, seat: Player) -> Result<i32, DomainError> {
        let new_heat = self.outfit_at(seat).starting_heat + HEAT_PER_PLAY;
        if !HEAT_BOUNDS.contains(&new_heat) {
            return Err(DomainError::InvariantViolation(format!(
                "playing this card would raise player {seat:?} Heat to {new_heat}, leaving the bounds [{}, {}]",
                HEAT_BOUNDS.start(),
                HEAT_BOUNDS.end()
            )));
        }
        Ok(new_heat)
    }

    /// Handle `PlayCardCmd`: verify the command targets this match, a real player
    /// whose turn it is, and a well-formed card/target; enforce every match-play
    /// invariant against the session's state; pay the card's Juice cost; and emit
    /// [`Event::CardPlayed`] followed by [`Event::HeatRaised`].
    fn play_card(&mut self, cmd: PlayCard) -> Result<Vec<Event>, DomainError> {
        // The command must name the match this session actually records.
        if cmd.match_id != self.match_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets match '{}' but this session records '{}'",
                cmd.match_id, self.match_id
            )));
        }
        // A player must be named, and it must be one of the configured Outfits.
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "a playerId must be provided".to_string(),
            ));
        }
        let seat = self.seat_for_player(&cmd.player_id)?;

        // The card being played and its target must both be identified.
        if cmd.card_instance_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "a cardInstanceId must be provided".to_string(),
            ));
        }
        if cmd.target_ref.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "a targetRef must be provided".to_string(),
            ));
        }

        // Enforce every match-play invariant before applying the play.
        self.ensure_boards_within_caps()?;
        self.ensure_heat_within_bounds()?;
        self.ensure_starting_juice_valid()?;
        self.ensure_decks_nonempty()?;
        self.ensure_heists_prereqs_satisfied()?;
        self.ensure_bosses_alive()?;
        let turn_player = self.ensure_opening_player_designated()?;

        // Turn-ownership: a card is played only by the player whose turn it is.
        if seat != turn_player {
            return Err(DomainError::InvariantViolation(format!(
                "player '{}' (seat {seat:?}) may not play a card; it is player {turn_player:?}'s turn",
                cmd.player_id
            )));
        }

        // Pay the card's Juice cost, and compute the Heat the play raises.
        self.ensure_card_affordable(seat, cmd.juice_cost)?;
        let new_heat = self.heat_after_play(seat)?;

        // A successful play emits the card first, then the Heat it raised.
        let played = Event::CardPlayed(CardPlayed {
            match_id: cmd.match_id.clone(),
            player_id: cmd.player_id,
            player: seat,
            card_instance_id: cmd.card_instance_id,
            target_ref: cmd.target_ref,
            juice_spent: cmd.juice_cost,
        });
        let raised = Event::HeatRaised(HeatRaised {
            match_id: cmd.match_id,
            player: seat,
            amount: HEAT_PER_PLAY,
            new_heat,
        });
        self.root.record(Box::new(played.clone()));
        self.root.record(Box::new(raised.clone()));
        Ok(vec![played, raised])
    }

    /// Handle `DeclareAttackCmd`: verify the command targets this match, a real
    /// turn-holding player, and well-formed attacker/defender references;
    /// enforce every match-play invariant; resolve combat simultaneously; and
    /// emit [`Event::CombatResolved`] followed by [`Event::BossDefeated`].
    fn declare_attack(&mut self, cmd: DeclareAttack) -> Result<Vec<Event>, DomainError> {
        // The command must name the match this session actually records.
        if cmd.match_id != self.match_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets match '{}' but this session records '{}'",
                cmd.match_id, self.match_id
            )));
        }
        // A player must be named, and it must be one of the configured Outfits.
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "a playerId must be provided".to_string(),
            ));
        }
        let seat = self.seat_for_player(&cmd.player_id)?;

        // Combat must name both sides. The rules engine slice does not yet carry
        // individual combatant stats, so the target id is the defeated Boss ref.
        if cmd.attacker_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "an attackerId must be provided".to_string(),
            ));
        }
        if cmd.defender_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "a defenderId must be provided".to_string(),
            ));
        }
        let defending_player = Self::opponent_of(seat);
        let expected_defender_id = self.outfit_at(defending_player).boss_name.clone();
        if cmd.defender_id != expected_defender_id {
            return Err(DomainError::InvariantViolation(format!(
                "defenderId '{}' does not name player {defending_player:?}'s Boss target '{}'",
                cmd.defender_id, expected_defender_id
            )));
        }

        // Enforce every match-play invariant before applying the attack.
        self.ensure_boards_within_caps()?;
        self.ensure_heat_within_bounds()?;
        self.ensure_starting_juice_valid()?;
        self.ensure_decks_nonempty()?;
        self.ensure_heists_prereqs_satisfied()?;
        self.ensure_bosses_alive()?;
        let turn_player = self.ensure_opening_player_designated()?;

        // Turn-ownership: an attack is declared only by the player whose turn it is.
        if seat != turn_player {
            return Err(DomainError::InvariantViolation(format!(
                "player '{}' (seat {seat:?}) may not declare an attack; it is player {turn_player:?}'s turn",
                cmd.player_id
            )));
        }

        let defeated_player_id = self.outfit_at(defending_player).name.clone();

        let resolved = Event::CombatResolved(CombatResolved {
            match_id: cmd.match_id.clone(),
            attacking_player_id: cmd.player_id,
            attacking_player: seat,
            attacker_id: cmd.attacker_id,
            defending_player,
            defender_id: cmd.defender_id.clone(),
        });
        let defeated = Event::BossDefeated(BossDefeated {
            match_id: cmd.match_id,
            defeated_player_id,
            defeated_player: defending_player,
            boss_id: cmd.defender_id,
            winner: seat,
        });

        // Apply the lethal combat result so the aggregate no longer carries a
        // live defending Boss after emitting the defeat.
        self.outfit_at_mut(defending_player).boss_hp = 0;

        self.root.record(Box::new(resolved.clone()));
        self.root.record(Box::new(defeated.clone()));
        Ok(vec![resolved, defeated])
    }
}

impl Aggregate for GameSession {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            START_MATCH => {
                let cmd: StartMatch = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed StartMatchCmd payload: {e}"))
                })?;
                self.start_match(cmd)
            }
            MULLIGAN => {
                let cmd: Mulligan = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed MulliganCmd payload: {e}"))
                })?;
                self.mulligan(cmd)
            }
            PLAY_CARD => {
                let cmd: PlayCard = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed PlayCardCmd payload: {e}"))
                })?;
                self.play_card(cmd)
            }
            DECLARE_ATTACK => {
                let cmd: DeclareAttack = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!(
                        "malformed DeclareAttackCmd payload: {e}"
                    ))
                })?;
                self.declare_attack(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`GameSession`] aggregate. Adapters implement
/// [`shared::Repository`] for `GameSession` and then this marker trait.
pub trait GameSessionRepository: Repository<GameSession> {}

/// WASM bindings for shared server/client rule execution.
///
/// Only compiled when the `wasm` feature is enabled (i.e. under `wasm-pack`),
/// so the native host build never pulls in `wasm-bindgen`.
#[cfg(feature = "wasm")]
mod wasm_bindings {
    use super::GameSession;
    use shared::{Aggregate, Command};
    use wasm_bindgen::prelude::*;

    /// Run a command against a fresh `GameSession` from the browser client.
    ///
    /// Returns `Ok(())` when the command is applied, or the domain error text
    /// (e.g. the `UnknownCommand` message, or an invariant violation) as a
    /// `JsValue` — mirroring exactly what the authoritative server would decide
    /// for the same input.
    #[wasm_bindgen]
    pub fn execute_command(session_id: String, command_name: String) -> Result<(), JsValue> {
        let mut session = GameSession::new(session_id);
        session
            .execute(Command::new(command_name))
            .map(|_events| ())
            .map_err(|err| JsValue::from_str(&err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session `m-1` set up as a legal opening: two default Outfits within all
    /// caps, healthy Bosses, and player `A` to move. Tests mutate one aspect at a
    /// time to drive a specific rejection.
    fn valid_session() -> GameSession {
        GameSession::new("m-1")
    }

    /// A `StartMatchCmd` opening `m-1` between its two configured Outfits.
    fn valid_cmd() -> StartMatch {
        StartMatch::new("m-1", "m-1-a", "m-1-b", 0xC0FFEE)
    }

    // Scenario: Successfully execute StartMatchCmd.
    #[test]
    fn starts_match_and_emits_match_started_event() {
        let mut session = valid_session();

        let events = session
            .execute(valid_cmd().into_command())
            .expect("a valid start should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "match.started");
        match &events[0] {
            Event::MatchStarted(started) => {
                assert_eq!(started.match_id, "m-1");
                assert_eq!(started.player_a_outfit, "m-1-a");
                assert_eq!(started.player_b_outfit, "m-1-b");
                assert_eq!(started.rng_seed, 0xC0FFEE);
                assert_eq!(started.opening_player, Player::A);
            }
            other => panic!("expected MatchStarted, got {other:?}"),
        }
        // The event was recorded on the aggregate root.
        assert_eq!(session.version(), 1);
        assert_eq!(session.uncommitted_events().len(), 1);
        assert_eq!(
            session.uncommitted_events()[0].event_type(),
            "match.started"
        );
    }

    // Scenario: rejected — Juice starts at 1 (hard-capped at 10).
    #[test]
    fn rejects_when_starting_juice_is_not_one() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_juice = 3; // Juice must open at STARTING_JUICE.
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_cmd().into_command())
            .expect_err("an illegal opening Juice must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a board may hold at most 7 Operators and 3 Vehicles.
    #[test]
    fn rejects_when_board_exceeds_operator_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.operators = MAX_OPERATORS + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_cmd().into_command())
            .expect_err("an over-capacity board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn rejects_when_board_exceeds_vehicle_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.vehicles = MAX_VEHICLES + 1;
        session.configure_player_b(outfit);
        // player_b's Outfit name is unchanged, so the command still matches.
        let err = session
            .execute(StartMatch::new("m-1", "m-1-a", "m-1-b", 7).into_command())
            .expect_err("an over-capacity vehicle board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Heat is bounded 0..10 and no state may leave it.
    #[test]
    fn rejects_when_heat_leaves_bounds() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_heat = *HEAT_BOUNDS.end() + 1; // Outside [0, 10].
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_cmd().into_command())
            .expect_err("Heat outside its bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a Heist resolves only after its prerequisite queue is
    // satisfied.
    #[test]
    fn rejects_when_heist_resolved_with_outstanding_prereqs() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.heist_resolved = true;
        outfit.outstanding_heist_prereqs = 2;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_cmd().into_command())
            .expect_err("a Heist resolved with outstanding prereqs must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — drawing from an empty deck deals Fatigue instead of a
    // card, so a match may not open deckless.
    #[test]
    fn rejects_when_deck_is_empty() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.deck_size = 0;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_cmd().into_command())
            .expect_err("an empty opening deck must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a command is valid only for the player whose turn it
    // currently is; a start with no designated opening player is ill-formed.
    #[test]
    fn rejects_when_no_opening_player_is_designated() {
        let mut session = valid_session();
        session.set_opening_player(None);

        let err = session
            .execute(valid_cmd().into_command())
            .expect_err("a turn-less start must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a match ends the instant a Boss's HP reaches 0 or
    // below, so a Boss cannot start a match already defeated.
    #[test]
    fn rejects_when_a_boss_starts_defeated() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.boss_hp = 0;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_cmd().into_command())
            .expect_err("a defeated Boss must be rejected at start");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A start must name the match this session records.
    #[test]
    fn rejects_when_command_targets_a_different_match() {
        let mut session = valid_session();
        let err = session
            .execute(StartMatch::new("other-match", "m-1-a", "m-1-b", 7).into_command())
            .expect_err("a mismatched match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A start needs a non-zero, reproducible seed.
    #[test]
    fn rejects_zero_rng_seed() {
        let mut session = valid_session();
        let err = session
            .execute(StartMatch::new("m-1", "m-1-a", "m-1-b", 0).into_command())
            .expect_err("a zero rng seed must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut session = GameSession::new("m-1");
        let err = session.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "GameSession");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, StartMatch::COMMAND);
        let decoded: StartMatch = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }

    // ---- MulliganCmd (S-3) --------------------------------------------------

    /// A legal `MulliganCmd` for `m-1`: the turn-holding player `A` redraws two
    /// distinct cards. Tests mutate one aspect at a time to drive a rejection.
    fn valid_mulligan() -> Mulligan {
        Mulligan::new("m-1", "m-1-a", ["card-1", "card-2"])
    }

    // Scenario: Successfully execute MulliganCmd.
    #[test]
    fn applies_mulligan_and_emits_mulligan_completed_event() {
        let mut session = valid_session();

        let events = session
            .execute(valid_mulligan().into_command())
            .expect("a valid mulligan should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "mulligan.completed");
        match &events[0] {
            Event::MulliganCompleted(done) => {
                assert_eq!(done.match_id, "m-1");
                assert_eq!(done.player_id, "m-1-a");
                assert_eq!(done.player, Player::A);
                assert_eq!(done.redrawn_card_ids, vec!["card-1", "card-2"]);
            }
            other => panic!("expected MulliganCompleted, got {other:?}"),
        }
        assert_eq!(session.version(), 1);
        assert_eq!(session.uncommitted_events().len(), 1);
        assert_eq!(
            session.uncommitted_events()[0].event_type(),
            "mulligan.completed"
        );
    }

    // A redraw of zero cards (keep the whole hand) is a legal mulligan.
    #[test]
    fn applies_empty_mulligan_keeping_the_hand() {
        let mut session = valid_session();
        let events = session
            .execute(Mulligan::new("m-1", "m-1-a", Vec::<String>::new()).into_command())
            .expect("keeping the whole hand is a valid mulligan");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "mulligan.completed");
        assert_eq!(session.version(), 1);
    }

    // Scenario: rejected — Juice starts at 1 (hard-capped at 10).
    #[test]
    fn mulligan_rejects_when_starting_juice_is_not_one() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_juice = 3;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_mulligan().into_command())
            .expect_err("an illegal opening Juice must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a board may hold at most 7 Operators and 3 Vehicles.
    #[test]
    fn mulligan_rejects_when_board_exceeds_operator_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.operators = MAX_OPERATORS + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_mulligan().into_command())
            .expect_err("an over-capacity board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn mulligan_rejects_when_board_exceeds_vehicle_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.vehicles = MAX_VEHICLES + 1;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_mulligan().into_command())
            .expect_err("an over-capacity vehicle board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Heat is bounded 0..10 and no state may leave it.
    #[test]
    fn mulligan_rejects_when_heat_leaves_bounds() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_heat = *HEAT_BOUNDS.end() + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_mulligan().into_command())
            .expect_err("Heat outside its bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a Heist resolves only after its prerequisite queue is
    // satisfied.
    #[test]
    fn mulligan_rejects_when_heist_resolved_with_outstanding_prereqs() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.heist_resolved = true;
        outfit.outstanding_heist_prereqs = 2;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_mulligan().into_command())
            .expect_err("a Heist resolved with outstanding prereqs must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — drawing from an empty deck deals Fatigue instead of a
    // card, so a match may not carry a deckless Outfit.
    #[test]
    fn mulligan_rejects_when_deck_is_empty() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.deck_size = 0;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_mulligan().into_command())
            .expect_err("an empty deck must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A redraw cannot ask for more cards than the deck can replace (drawing past
    // an empty deck deals Fatigue instead of a card).
    #[test]
    fn mulligan_rejects_when_redraw_exceeds_deck() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.deck_size = 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(Mulligan::new("m-1", "m-1-a", ["c1", "c2"]).into_command())
            .expect_err("redrawing more cards than the deck can replace must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a command is valid only for the player whose turn it
    // currently is; a mulligan by the off-turn player is rejected.
    #[test]
    fn mulligan_rejects_when_not_the_players_turn() {
        let mut session = valid_session();
        // Player A holds the turn; player B tries to mulligan.
        let err = session
            .execute(Mulligan::new("m-1", "m-1-b", ["card-1"]).into_command())
            .expect_err("an off-turn mulligan must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A turn-less setup is likewise ill-formed for a mulligan.
    #[test]
    fn mulligan_rejects_when_no_opening_player_is_designated() {
        let mut session = valid_session();
        session.set_opening_player(None);

        let err = session
            .execute(valid_mulligan().into_command())
            .expect_err("a turn-less setup must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a match ends the instant a Boss's HP reaches 0 or
    // below, so a defeated Boss cannot be carried into a mulligan.
    #[test]
    fn mulligan_rejects_when_a_boss_is_defeated() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.boss_hp = 0;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_mulligan().into_command())
            .expect_err("a defeated Boss must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A mulligan must name the match this session records.
    #[test]
    fn mulligan_rejects_when_command_targets_a_different_match() {
        let mut session = valid_session();
        let err = session
            .execute(Mulligan::new("other-match", "m-1-a", ["card-1"]).into_command())
            .expect_err("a mismatched match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A mulligan must name a configured Outfit.
    #[test]
    fn mulligan_rejects_unknown_player() {
        let mut session = valid_session();
        let err = session
            .execute(Mulligan::new("m-1", "ghost", ["card-1"]).into_command())
            .expect_err("an unknown player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A redraw selection must not contain duplicate card ids.
    #[test]
    fn mulligan_rejects_duplicate_card_ids() {
        let mut session = valid_session();
        let err = session
            .execute(Mulligan::new("m-1", "m-1-a", ["dup", "dup"]).into_command())
            .expect_err("duplicate card ids must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn mulligan_command_payload_round_trips() {
        let cmd = valid_mulligan();
        let command = cmd.into_command();
        assert_eq!(command.name, Mulligan::COMMAND);
        let decoded: Mulligan = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_mulligan());
    }

    // ---- PlayCardCmd (S-4) --------------------------------------------------

    /// A legal `PlayCardCmd` for `m-1`: the turn-holding player `A` plays a
    /// card instance at a target, paying 2 Juice (within the default available
    /// pool of 3). Tests mutate one aspect at a time to drive a rejection.
    fn valid_play_card() -> PlayCard {
        PlayCard::new("m-1", "m-1-a", "card-instance-1", "target-1", 2)
    }

    // Scenario: Successfully execute PlayCardCmd — a card.played event and a
    // heat.raised event are emitted.
    #[test]
    fn plays_card_and_emits_card_played_and_heat_raised_events() {
        let mut session = valid_session();

        let events = session
            .execute(valid_play_card().into_command())
            .expect("a valid play should succeed");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type(), "card.played");
        assert_eq!(events[1].event_type(), "heat.raised");
        match &events[0] {
            Event::CardPlayed(played) => {
                assert_eq!(played.match_id, "m-1");
                assert_eq!(played.player_id, "m-1-a");
                assert_eq!(played.player, Player::A);
                assert_eq!(played.card_instance_id, "card-instance-1");
                assert_eq!(played.target_ref, "target-1");
                assert_eq!(played.juice_spent, 2);
            }
            other => panic!("expected CardPlayed, got {other:?}"),
        }
        match &events[1] {
            Event::HeatRaised(raised) => {
                assert_eq!(raised.match_id, "m-1");
                assert_eq!(raised.player, Player::A);
                assert_eq!(raised.amount, HEAT_PER_PLAY);
                // Default opening Heat is 0, raised by HEAT_PER_PLAY.
                assert_eq!(raised.new_heat, HEAT_PER_PLAY);
            }
            other => panic!("expected HeatRaised, got {other:?}"),
        }
        // Two events recorded on the root: the version advances by two.
        assert_eq!(session.version(), 2);
        assert_eq!(session.uncommitted_events().len(), 2);
        assert_eq!(session.uncommitted_events()[0].event_type(), "card.played");
        assert_eq!(session.uncommitted_events()[1].event_type(), "heat.raised");
    }

    // Scenario: rejected — a card may only be played when its Juice cost does not
    // exceed currently available Juice.
    #[test]
    fn play_card_rejects_when_cost_exceeds_available_juice() {
        let mut session = valid_session();
        // Default available Juice is 3; a cost of 4 cannot be afforded.
        let err = session
            .execute(PlayCard::new("m-1", "m-1-a", "card-instance-1", "target-1", 4).into_command())
            .expect_err("a card the player cannot afford must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a board may hold at most 7 Operators and 3 Vehicles.
    #[test]
    fn play_card_rejects_when_board_exceeds_operator_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.operators = MAX_OPERATORS + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_play_card().into_command())
            .expect_err("an over-capacity board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn play_card_rejects_when_board_exceeds_vehicle_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.vehicles = MAX_VEHICLES + 1;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_play_card().into_command())
            .expect_err("an over-capacity vehicle board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Heat is bounded 0..10 and no state may leave it.
    #[test]
    fn play_card_rejects_when_heat_leaves_bounds() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_heat = *HEAT_BOUNDS.end() + 1; // Outside [0, 10].
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_play_card().into_command())
            .expect_err("Heat outside its bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a Heist resolves only after its prerequisite queue is
    // satisfied.
    #[test]
    fn play_card_rejects_when_heist_resolved_with_outstanding_prereqs() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.heist_resolved = true;
        outfit.outstanding_heist_prereqs = 2;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_play_card().into_command())
            .expect_err("a Heist resolved with outstanding prereqs must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — drawing from an empty deck deals Fatigue instead of a
    // card, so a match may not carry a deckless Outfit.
    #[test]
    fn play_card_rejects_when_deck_is_empty() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.deck_size = 0;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_play_card().into_command())
            .expect_err("an empty deck must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a command is valid only for the player whose turn it
    // currently is; a play by the off-turn player is rejected.
    #[test]
    fn play_card_rejects_when_not_the_players_turn() {
        let mut session = valid_session();
        // Player A holds the turn; player B tries to play a card.
        let err = session
            .execute(PlayCard::new("m-1", "m-1-b", "card-instance-1", "target-1", 2).into_command())
            .expect_err("an off-turn play must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A turn-less setup is likewise ill-formed for a play.
    #[test]
    fn play_card_rejects_when_no_opening_player_is_designated() {
        let mut session = valid_session();
        session.set_opening_player(None);

        let err = session
            .execute(valid_play_card().into_command())
            .expect_err("a turn-less setup must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a match ends the instant a Boss's HP reaches 0 or
    // below, so a defeated Boss cannot be carried into a play.
    #[test]
    fn play_card_rejects_when_a_boss_is_defeated() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.boss_hp = 0;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_play_card().into_command())
            .expect_err("a defeated Boss must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Juice starts at 1; an opening Juice that is not the
    // starting value is an illegal Juice state.
    #[test]
    fn play_card_rejects_when_starting_juice_is_not_one() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_juice = 3;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_play_card().into_command())
            .expect_err("an illegal opening Juice must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A play must name the match this session records.
    #[test]
    fn play_card_rejects_when_command_targets_a_different_match() {
        let mut session = valid_session();
        let err = session
            .execute(
                PlayCard::new("other-match", "m-1-a", "card-instance-1", "target-1", 2)
                    .into_command(),
            )
            .expect_err("a mismatched match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A play must name a configured Outfit.
    #[test]
    fn play_card_rejects_unknown_player() {
        let mut session = valid_session();
        let err = session
            .execute(PlayCard::new("m-1", "ghost", "card-instance-1", "target-1", 2).into_command())
            .expect_err("an unknown player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A play must identify the card instance being played.
    #[test]
    fn play_card_rejects_blank_card_instance() {
        let mut session = valid_session();
        let err = session
            .execute(PlayCard::new("m-1", "m-1-a", "  ", "target-1", 2).into_command())
            .expect_err("a blank cardInstanceId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A play must identify the target it resolves against.
    #[test]
    fn play_card_rejects_blank_target_ref() {
        let mut session = valid_session();
        let err = session
            .execute(PlayCard::new("m-1", "m-1-a", "card-instance-1", "", 2).into_command())
            .expect_err("a blank targetRef must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn play_card_command_payload_round_trips() {
        let cmd = valid_play_card();
        let command = cmd.into_command();
        assert_eq!(command.name, PlayCard::COMMAND);
        let decoded: PlayCard = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_play_card());
    }

    // ---- DeclareAttackCmd (S-5) --------------------------------------------

    /// A legal `DeclareAttackCmd` for `m-1`: the turn-holding player `A`
    /// commits an attacker into player `B`'s Boss target.
    fn valid_declare_attack() -> DeclareAttack {
        DeclareAttack::new("m-1", "m-1-a", "attacker-1", "m-1-b-boss")
    }

    // Scenario: Successfully execute DeclareAttackCmd — combat.resolved and
    // boss.defeated are emitted in order.
    #[test]
    fn declares_attack_and_emits_combat_resolved_and_boss_defeated_events() {
        let mut session = valid_session();

        let events = session
            .execute(valid_declare_attack().into_command())
            .expect("a valid attack should succeed");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type(), "combat.resolved");
        assert_eq!(events[1].event_type(), "boss.defeated");
        match &events[0] {
            Event::CombatResolved(resolved) => {
                assert_eq!(resolved.match_id, "m-1");
                assert_eq!(resolved.attacking_player_id, "m-1-a");
                assert_eq!(resolved.attacking_player, Player::A);
                assert_eq!(resolved.attacker_id, "attacker-1");
                assert_eq!(resolved.defending_player, Player::B);
                assert_eq!(resolved.defender_id, "m-1-b-boss");
            }
            other => panic!("expected CombatResolved, got {other:?}"),
        }
        match &events[1] {
            Event::BossDefeated(defeated) => {
                assert_eq!(defeated.match_id, "m-1");
                assert_eq!(defeated.defeated_player_id, "m-1-b");
                assert_eq!(defeated.defeated_player, Player::B);
                assert_eq!(defeated.boss_id, "m-1-b-boss");
                assert_eq!(defeated.winner, Player::A);
            }
            other => panic!("expected BossDefeated, got {other:?}"),
        }
        assert_eq!(session.version(), 2);
        assert_eq!(session.uncommitted_events().len(), 2);
        assert_eq!(
            session.uncommitted_events()[0].event_type(),
            "combat.resolved"
        );
        assert_eq!(
            session.uncommitted_events()[1].event_type(),
            "boss.defeated"
        );

        let err = session
            .execute(valid_declare_attack().into_command())
            .expect_err("a defeated Boss must end the match before another attack");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 2);
    }

    // Scenario: rejected — Juice starts at 1 and remains hard-capped at 10.
    #[test]
    fn declare_attack_rejects_when_starting_juice_is_not_one() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_juice = 3;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_declare_attack().into_command())
            .expect_err("an illegal opening Juice must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn declare_attack_rejects_when_available_juice_exceeds_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.available_juice = JUICE_CAP + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_declare_attack().into_command())
            .expect_err("available Juice over the hard cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a board may hold at most 7 Operators and 3 Vehicles.
    #[test]
    fn declare_attack_rejects_when_board_exceeds_operator_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.operators = MAX_OPERATORS + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_declare_attack().into_command())
            .expect_err("an over-capacity board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn declare_attack_rejects_when_board_exceeds_vehicle_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.vehicles = MAX_VEHICLES + 1;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_declare_attack().into_command())
            .expect_err("an over-capacity vehicle board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Heat is bounded 0..10 and no state may leave it.
    #[test]
    fn declare_attack_rejects_when_heat_leaves_bounds() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_heat = *HEAT_BOUNDS.end() + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_declare_attack().into_command())
            .expect_err("Heat outside its bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a Heist resolves only after its prerequisite queue is
    // satisfied.
    #[test]
    fn declare_attack_rejects_when_heist_resolved_with_outstanding_prereqs() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.heist_resolved = true;
        outfit.outstanding_heist_prereqs = 2;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_declare_attack().into_command())
            .expect_err("a Heist resolved with outstanding prereqs must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — drawing from an empty deck deals Fatigue instead of a
    // card, so a match may not carry a deckless Outfit.
    #[test]
    fn declare_attack_rejects_when_deck_is_empty() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.deck_size = 0;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_declare_attack().into_command())
            .expect_err("an empty deck must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a command is valid only for the player whose turn it
    // currently is; an attack by the off-turn player is rejected.
    #[test]
    fn declare_attack_rejects_when_not_the_players_turn() {
        let mut session = valid_session();
        // Player A holds the turn; player B tries to declare an attack.
        let err = session
            .execute(DeclareAttack::new("m-1", "m-1-b", "attacker-1", "m-1-a-boss").into_command())
            .expect_err("an off-turn attack must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A turn-less setup is likewise ill-formed for an attack.
    #[test]
    fn declare_attack_rejects_when_no_opening_player_is_designated() {
        let mut session = valid_session();
        session.set_opening_player(None);

        let err = session
            .execute(valid_declare_attack().into_command())
            .expect_err("a turn-less setup must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a match ends the instant a Boss's HP reaches 0 or
    // below, so a defeated Boss cannot be carried into an attack.
    #[test]
    fn declare_attack_rejects_when_a_boss_is_defeated() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.boss_hp = 0;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_declare_attack().into_command())
            .expect_err("a defeated Boss must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An attack must name the match this session records.
    #[test]
    fn declare_attack_rejects_when_command_targets_a_different_match() {
        let mut session = valid_session();
        let err = session
            .execute(
                DeclareAttack::new("other-match", "m-1-a", "attacker-1", "m-1-b-boss")
                    .into_command(),
            )
            .expect_err("a mismatched match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An attack must name a configured Outfit.
    #[test]
    fn declare_attack_rejects_unknown_player() {
        let mut session = valid_session();
        let err = session
            .execute(DeclareAttack::new("m-1", "ghost", "attacker-1", "m-1-b-boss").into_command())
            .expect_err("an unknown player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An attack must identify the attacking combatant.
    #[test]
    fn declare_attack_rejects_blank_attacker_id() {
        let mut session = valid_session();
        let err = session
            .execute(DeclareAttack::new("m-1", "m-1-a", " ", "m-1-b-boss").into_command())
            .expect_err("a blank attackerId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An attack must identify the defending target.
    #[test]
    fn declare_attack_rejects_blank_defender_id() {
        let mut session = valid_session();
        let err = session
            .execute(DeclareAttack::new("m-1", "m-1-a", "attacker-1", "").into_command())
            .expect_err("a blank defenderId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An attack that resolves into boss.defeated must target the opposing Boss.
    #[test]
    fn declare_attack_rejects_non_opposing_boss_defender_id() {
        let mut session = valid_session();
        let err = session
            .execute(DeclareAttack::new("m-1", "m-1-a", "attacker-1", "target-1").into_command())
            .expect_err("a non-Boss defenderId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn declare_attack_command_payload_round_trips() {
        let cmd = valid_declare_attack();
        let command = cmd.into_command();
        assert_eq!(command.name, DeclareAttack::COMMAND);
        let decoded: DeclareAttack = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_declare_attack());
    }
}
