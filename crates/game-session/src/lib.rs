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
//! [`Attack`] (`AttackCmd`) then commits the turn-holding player's ready
//! attacker at a `target_ref` (`"boss:<seat>"` | `"op:<instance_id>"`) and
//! resolves real board combat: the attacker deals its atk to the target and, if
//! the target is a unit, takes simultaneous retaliation. It emits
//! `operator.damaged`/`operator.died`/`operator.exhausted` deltas over the
//! [`BoardUnit`]s, and `boss.damaged` (with [`Event::BossDefeated`],
//! `boss.defeated`, at 0 HP) against a Boss target.
//!
//! [`ActivateHeroPower`] (`ActivateHeroPowerCmd`) then activates the turn-holding
//! player's Boss trademark hero power at a target: it pays the power's Juice cost
//! out of the seat's available pool (rejecting a power the player cannot afford),
//! re-checks the same rules-contract invariants, and on success emits
//! [`Event::HeroPowerActivated`] (`hero_power.activated`).
//!
//! [`EndTurn`] (`EndTurnCmd`) then passes the turn from the turn-holding player to
//! their opponent: it re-checks the same rules-contract invariants, ramps the
//! incoming seat's Juice (+1, hard-capped at [`JUICE_CAP`]), and resolves that
//! seat's start-of-turn draw — a card when the deck is non-empty, escalating
//! Fatigue to the drawing Boss when it is not. On success it emits *two* events,
//! [`Event::FatigueDamageDealt`] (`fatigue.damage.dealt`) followed by
//! [`Event::TurnEnded`] (`turn.ended`).
//!
//! [`ResolveCopEvent`] (`ResolveCopEventCmd`) then resolves the Cop Event that
//! fires when a seat's Heat hits [`HEAT_BOUNDS`]'s upper bound: it validates the
//! seeded d10 draw ([`COP_EVENT_DIE_SIDES`]) against the turn-holding player,
//! re-checks the same rules-contract invariants, resets that seat's Heat per the
//! rules-contract, and on success emits [`Event::CopEventTriggered`]
//! (`cop.event.triggered`).
//!
//! [`ConcedeMatch`] (`ConcedeMatchCmd`) then forfeits the match for the conceding
//! player. Concede is the one command the rules-contract exempts from the
//! whose-turn-it-is rule, so it validates against a real, well-formed match and
//! re-checks the same rules-contract invariants but — unlike every other command
//! — does *not* require the conceding player to hold the turn. On success the
//! opposing seat is declared the sole winner and it emits
//! [`Event::MatchCompleted`] (`match.completed`).

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use domain::boss_definition::{HeroPowerEffect, TrademarkEffect, TrademarkTrigger};
use domain::card_definition::{CardClass, CardType};
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

/// The `AttackCmd` command name [`GameSession::execute`] recognizes.
const ATTACK: &str = "AttackCmd";

/// The `ActivateHeroPowerCmd` command name [`GameSession::execute`] recognizes.
const ACTIVATE_HERO_POWER: &str = "ActivateHeroPowerCmd";

/// The `EndTurnCmd` command name [`GameSession::execute`] recognizes.
const END_TURN: &str = "EndTurnCmd";

/// The `ResolveCopEventCmd` command name [`GameSession::execute`] recognizes.
const RESOLVE_COP_EVENT: &str = "ResolveCopEventCmd";

/// The `ConcedeMatchCmd` command name [`GameSession::execute`] recognizes.
const CONCEDE_MATCH: &str = "ConcedeMatchCmd";

/// The `ResolveVenueEventCmd` command name [`GameSession::execute`]
/// recognizes (Task 10, City-pillar hook).
const RESOLVE_VENUE_EVENT: &str = "ResolveVenueEventCmd";

/// Heat a player gains each time they play a card. Playing a card always raises
/// Heat, so a successful [`PlayCard`] emits an accompanying `heat.raised` event.
pub const HEAT_PER_PLAY: i32 = 1;

/// A player's board may hold at most this many Operators simultaneously.
pub const MAX_OPERATORS: usize = 7;

/// A player's board may hold at most this many Vehicles simultaneously.
pub const MAX_VEHICLES: usize = 3;

/// Damage a Drive-By summon strafes at the enemy Boss on arrival. The client
/// keys Drive-By off the card's `amount` field (2 for Stolen Whip,
/// web/src/match/rules.ts:61/313), which [`CardEffect::Summon`] does not carry;
/// for Subsystem 1 the only Drive-By card uses 2, so a fixed constant matches.
/// Subsystem 2 makes it data-driven when the keyword catalog grows.
pub const DRIVE_BY_DAMAGE: u8 = 2;

/// Cards dealt to each seat's opening hand at match start (matches the client's
/// `OPENING_HAND`, web/src/match/rules.ts:68).
pub const OPENING_HAND: usize = 4;

/// Juice a player starts a match with (it ramps +1 each of the owner's turns).
pub const STARTING_JUICE: u8 = 1;

/// Juice is hard-capped at this value; no state may exceed it.
pub const JUICE_CAP: u8 = 10;

/// Juice a seat gains at the start of each of its turns; ending a turn ramps the
/// incoming seat's available Juice by this much (hard-capped at [`JUICE_CAP`]).
pub const JUICE_RAMP_PER_TURN: u8 = 1;

/// Fatigue dealt to the drawing Boss by a draw from an empty deck. Drawing from
/// an empty deck deals escalating Fatigue instead of yielding a card; this is the
/// base increment. The deck-nonempty invariant means a legal end of turn never
/// reaches this, but the start-of-turn draw resolution models the rule faithfully.
pub const FATIGUE_PER_EMPTY_DRAW: i32 = 1;

/// Heat is bounded to this inclusive range; no state may leave it. Reaching the
/// upper bound immediately triggers a Cop Event, so a *clean* match start must
/// sit strictly below it (see [`GameSession::ensure_heat_within_bounds`]).
pub const HEAT_BOUNDS: RangeInclusive<i32> = 0..=10;

/// The seeded Cop Event table is a d10: a Cop Event draw is a face `1..=10`.
/// Reaching the upper Heat bound triggers a Cop Event, which is then resolved by
/// drawing from this table; a draw outside `1..=COP_EVENT_DIE_SIDES` is not a
/// valid d10 result.
pub const COP_EVENT_DIE_SIDES: u8 = 10;

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
    /// The seat's max-Juice "crystal": the ceiling `available_juice` refills to
    /// at the start of each of the owner's turns. Grows by `JUICE_RAMP_PER_TURN`
    /// each of the owner's turns, hard-capped at `JUICE_CAP`, INDEPENDENT of spend.
    pub max_juice: u8,
    /// Whether a Heist has been marked resolved for this Outfit at start.
    pub heist_resolved: bool,
    /// Outstanding prerequisites in this Outfit's Heist prerequisite queue. A
    /// Heist may only resolve once this reaches zero.
    pub outstanding_heist_prereqs: usize,
    /// The Boss's typed, resolvable hero-power effect (`domain::boss_definition`
    /// design note): threaded onto the Outfit at match configuration so
    /// [`GameSession::activate_hero_power`] can resolve it without a
    /// cross-aggregate lookup. Defaults to `DealDamage { amount: 2 }`, matching
    /// the client's hardcoded 2-poke (web/src/match/rules.ts:250).
    pub hero_power_effect: HeroPowerEffect,
    /// The Boss hero power's declared Juice cost. A command activating the
    /// power must claim exactly this cost (see
    /// [`GameSession::activate_hero_power`]'s anti-cheat cost-parity check).
    pub hero_power_cost: u8,
    /// The Boss's typed trademark effect, if any. `None` (the default) is a
    /// no-op — [`GameSession::end_turn`]'s start-of-turn trademark seam only
    /// fires when this is `Some` with `trigger: StartOfTurn`. The trademark
    /// catalog itself is Subsystem 2.
    pub trademark_effect: Option<TrademarkEffect>,
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
            max_juice: 3,
            heist_resolved: false,
            outstanding_heist_prereqs: 0,
            hero_power_effect: HeroPowerEffect::DealDamage { amount: 2 },
            hero_power_cost: 2,
            trademark_effect: None,
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
    /// The venue this match is played at (Task 10, City-pillar hook). Absent
    /// from existing `StartMatchCmd` payloads deserializes to `None` (see
    /// [`GameSession::apply_location_modifiers`]'s identity behavior), so this
    /// is additive: the command path can now carry a location, matching the
    /// [`GameSession::set_location`] server-side config method.
    #[serde(default)]
    pub location: Option<LocationModifier>,
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
            location: None,
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

/// The `AttackCmd` payload: the match being played, the player declaring the
/// attack, the attacker they are committing, and the target reference being
/// attacked. Field names are the match-play schema's `camelCase`.
///
/// `target_ref` resolves a combat target: `"boss:<seat>"` names the enemy Boss,
/// `"op:<instance_id>"` names an enemy board unit.
///
/// Build one directly and turn it into a [`Command`] with
/// [`Attack::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`GameSession::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attack {
    /// Identifier of the match being played; must name the match this session
    /// records.
    pub match_id: String,
    /// Identity of the player declaring the attack; must name one of this
    /// session's configured Outfits, and it must be that player's turn.
    pub player_id: String,
    /// The attacking combatant — a ready unit on the acting seat's board.
    pub attacker_id: String,
    /// The combat target: `"boss:<seat>"` for the enemy Boss, or
    /// `"op:<instance_id>"` for an enemy board unit.
    pub target_ref: String,
}

impl Attack {
    /// The command name this maps to.
    pub const COMMAND: &'static str = ATTACK;

    /// Build an `AttackCmd` for `player_id` in `match_id`, committing
    /// `attacker_id` against `target_ref`.
    pub fn new(
        match_id: impl Into<String>,
        player_id: impl Into<String>,
        attacker_id: impl Into<String>,
        target_ref: impl Into<String>,
    ) -> Self {
        Self {
            match_id: match_id.into(),
            player_id: player_id.into(),
            attacker_id: attacker_id.into(),
            target_ref: target_ref.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`GameSession::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("Attack is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `ActivateHeroPowerCmd` payload: the match being played, the player using
/// their Boss's trademark hero power, the target the power resolves against, and
/// the hero power's Juice cost. Field names are the match-play schema's
/// `camelCase`.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ActivateHeroPower::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`GameSession::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateHeroPower {
    /// Identifier of the match being played; must name the match this session
    /// records.
    pub match_id: String,
    /// Identity of the player activating the hero power; must name one of this
    /// session's configured Outfits, and it must be that player's turn.
    pub player_id: String,
    /// A reference to the target the hero power resolves against. Must be
    /// non-blank.
    pub target_ref: String,
    /// The hero power's Juice cost. A hero power may only be activated when its
    /// cost does not exceed the player's currently available Juice.
    pub juice_cost: u8,
}

impl ActivateHeroPower {
    /// The command name this maps to.
    pub const COMMAND: &'static str = ACTIVATE_HERO_POWER;

    /// Build an `ActivateHeroPowerCmd` for `player_id` in `match_id`, resolving
    /// the Boss hero power against `target_ref` and paying `juice_cost` Juice.
    pub fn new(
        match_id: impl Into<String>,
        player_id: impl Into<String>,
        target_ref: impl Into<String>,
        juice_cost: u8,
    ) -> Self {
        Self {
            match_id: match_id.into(),
            player_id: player_id.into(),
            target_ref: target_ref.into(),
            juice_cost,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`GameSession::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ActivateHeroPower is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `EndTurnCmd` payload: the match being played and the player passing the
/// turn. Field names are the match-play schema's `camelCase`.
///
/// Build one directly and turn it into a [`Command`] with
/// [`EndTurn::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`GameSession::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndTurn {
    /// Identifier of the match being played; must name the match this session
    /// records.
    pub match_id: String,
    /// Identity of the player ending their turn; must name one of this session's
    /// configured Outfits, and it must be that player's turn.
    pub player_id: String,
}

impl EndTurn {
    /// The command name this maps to.
    pub const COMMAND: &'static str = END_TURN;

    /// Build an `EndTurnCmd` for `player_id` in `match_id`.
    pub fn new(match_id: impl Into<String>, player_id: impl Into<String>) -> Self {
        Self {
            match_id: match_id.into(),
            player_id: player_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`GameSession::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("EndTurn is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `ResolveCopEventCmd` payload: the match being played, the player whose
/// Heat hit the Cop-Event threshold, and the seeded d10 draw that selects the Cop
/// Event from the table. Field names are the match-play schema's `camelCase`.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ResolveCopEvent::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`GameSession::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveCopEvent {
    /// Identifier of the match being played; must name the match this session
    /// records.
    pub match_id: String,
    /// Identity of the player resolving the Cop Event; must name one of this
    /// session's configured Outfits, and it must be that player's turn.
    pub player_id: String,
    /// The seeded d10 draw that selects the Cop Event from the table. Must be a
    /// valid d10 face, `1..=`[`COP_EVENT_DIE_SIDES`].
    pub rng_draw: u8,
}

impl ResolveCopEvent {
    /// The command name this maps to.
    pub const COMMAND: &'static str = RESOLVE_COP_EVENT;

    /// Build a `ResolveCopEventCmd` for `player_id` in `match_id`, resolving the
    /// Cop Event selected by the seeded d10 `rng_draw`.
    pub fn new(match_id: impl Into<String>, player_id: impl Into<String>, rng_draw: u8) -> Self {
        Self {
            match_id: match_id.into(),
            player_id: player_id.into(),
            rng_draw,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`GameSession::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ResolveCopEvent is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `ConcedeMatchCmd` payload: the match being played and the player
/// forfeiting it. Field names are the match-play schema's `camelCase`.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ConcedeMatch::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`GameSession::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConcedeMatch {
    /// Identifier of the match being played; must name the match this session
    /// records.
    pub match_id: String,
    /// Identity of the player forfeiting the match; must name one of this
    /// session's configured Outfits. Unlike every other command, concede is
    /// valid whether or not it is this player's turn.
    pub player_id: String,
}

impl ConcedeMatch {
    /// The command name this maps to.
    pub const COMMAND: &'static str = CONCEDE_MATCH;

    /// Build a `ConcedeMatchCmd` forfeiting `match_id` for `player_id`.
    pub fn new(match_id: impl Into<String>, player_id: impl Into<String>) -> Self {
        Self {
            match_id: match_id.into(),
            player_id: player_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`GameSession::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ConcedeMatch is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `ResolveVenueEventCmd` payload (Task 10, City-pillar hook): the match
/// being played, a reference into the venue's event table, and the seeded RNG
/// draw that selects the entry. Unlike [`ResolveCopEvent`], a venue event is
/// a neutral, match-level draw — it names no acting player and is not gated
/// on whose turn it is. Field names are the match-play schema's `camelCase`.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ResolveVenueEvent::into_command`], or decode it from a command payload
/// via [`serde_json`] inside [`GameSession::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveVenueEvent {
    /// Identifier of the match being played; must name the match this session
    /// records.
    pub match_id: String,
    /// Reference into the venue's event table this draw resolves against. In
    /// Subsystem 1 the table is a single no-op entry; Subsystem 3 grows it
    /// with real venue events.
    pub event_table_ref: String,
    /// The seeded RNG draw that selects the entry from the venue event table.
    pub rng_draw: u8,
}

impl ResolveVenueEvent {
    /// The command name this maps to.
    pub const COMMAND: &'static str = RESOLVE_VENUE_EVENT;

    /// Build a `ResolveVenueEventCmd` for `match_id`, resolving the venue
    /// event selected by the seeded `rng_draw` from `event_table_ref`.
    pub fn new(
        match_id: impl Into<String>,
        event_table_ref: impl Into<String>,
        rng_draw: u8,
    ) -> Self {
        Self {
            match_id: match_id.into(),
            event_table_ref: event_table_ref.into(),
            rng_draw,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`GameSession::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ResolveVenueEvent is always serializable");
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

/// A board unit that took combat damage, carried by [`Event::OperatorDamaged`]
/// and thus by the emitted `operator.damaged` event. `player` is the owner of
/// the damaged unit (the defender in a trade).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorDamaged {
    /// The match the damage happened in.
    pub match_id: String,
    /// The seat that owns the damaged unit.
    pub player: Player,
    /// The instance id of the damaged unit.
    pub instance_id: String,
    /// The unit's HP after the damage was applied (saturating at 0).
    pub new_hp: u8,
}

/// A board unit destroyed by combat (HP reached 0), carried by
/// [`Event::OperatorDied`] and thus by the emitted `operator.died` event.
/// `player` is the owner of the destroyed unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorDied {
    /// The match the unit died in.
    pub match_id: String,
    /// The seat that owned the destroyed unit.
    pub player: Player,
    /// The instance id of the destroyed unit.
    pub instance_id: String,
}

/// A Boss that took combat damage, carried by [`Event::BossDamaged`] and thus by
/// the emitted `boss.damaged` event. `player` is the owner of the damaged Boss
/// (the defender).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BossDamaged {
    /// The match the damage happened in.
    pub match_id: String,
    /// The seat whose Boss was damaged.
    pub player: Player,
    /// The damage dealt to the Boss.
    pub amount: i32,
    /// The Boss's HP after the damage was applied (clamped at 0).
    pub new_hp: i32,
}

/// A board unit that spent its attack and can no longer act this turn, carried
/// by [`Event::OperatorExhausted`] and thus by the emitted `operator.exhausted`
/// event. `player` is the owner (the attacker) of the exhausted unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorExhausted {
    /// The match the attack happened in.
    pub match_id: String,
    /// The seat that owns the exhausted unit.
    pub player: Player,
    /// The instance id of the exhausted unit.
    pub instance_id: String,
}

/// A unit put on the board by a Summon effect, carried by
/// [`Event::OperatorSummoned`] and thus by the emitted `operator.summoned`
/// event. Mirrors the client's summon fold (web/src/match/model.ts:232). The
/// summoned unit arrives unready (`ready: false`, summoning sickness).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorSummoned {
    /// The match the unit was summoned in.
    pub match_id: String,
    /// The seat that summoned the unit.
    pub player: Player,
    /// The unit placed on the board (unready the turn it arrives).
    pub unit: BoardUnit,
}

/// An activated Boss hero power, carried by [`Event::HeroPowerActivated`] and
/// thus by the emitted `hero_power.activated` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeroPowerActivated {
    /// The match the hero power was activated in.
    pub match_id: String,
    /// The player identity that activated the hero power.
    pub player_id: String,
    /// The seat that player occupies.
    pub player: Player,
    /// The target the hero power resolved against.
    pub target_ref: String,
    /// The Juice paid to activate the hero power.
    pub juice_spent: u8,
    /// The seat's remaining available Juice after paying the hero power's cost.
    pub remaining_juice: u8,
}

/// A resolved start-of-turn draw, carried by [`Event::FatigueDamageDealt`] and
/// thus by the emitted `fatigue.damage.dealt` event. Ending a turn resolves the
/// incoming seat's start-of-turn draw: a non-empty deck yields a card and deals
/// no Fatigue, so `amount` is `0`; drawing from an empty deck would instead deal
/// escalating Fatigue to the drawing Boss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FatigueDamageDealt {
    /// The match the draw happened in.
    pub match_id: String,
    /// The player identity of the incoming seat that drew.
    pub player_id: String,
    /// The incoming seat that drew at the start of its turn.
    pub player: Player,
    /// Fatigue dealt to the drawing Boss (`0` when a card was available).
    pub amount: i32,
    /// The drawing Boss's HP after any Fatigue was applied (always `> 0`; the
    /// deck-nonempty invariant leaves it unchanged on a legal end of turn).
    pub boss_hp_remaining: i32,
}

/// The incoming seat's board units readied at the start of its turn, carried
/// by [`Event::OperatorsReadied`] and thus by the emitted `operators.readied`
/// event. Clears summoning sickness so a unit summoned last turn can attack
/// this turn (spec §1b step 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorsReadied {
    /// The match the readying happened in.
    pub match_id: String,
    /// The incoming seat whose units were readied at its turn start.
    pub player: Player,
}

/// A passed turn, carried by [`Event::TurnEnded`] and thus by the emitted
/// `turn.ended` event. The turn passes from the ending seat to its opponent,
/// whose available Juice ramps for the turn now beginning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEnded {
    /// The match the turn was passed in.
    pub match_id: String,
    /// The player identity that ended their turn.
    pub player_id: String,
    /// The seat that ended its turn.
    pub player: Player,
    /// The seat whose turn now begins.
    pub next_player: Player,
    /// The incoming seat's available Juice after ramping (+1, hard-capped at
    /// [`JUICE_CAP`]).
    pub next_player_juice: u8,
    /// The incoming seat's grown max-Juice crystal (what `next_player_juice`
    /// refills to). Lets the client render the crystal, not just the pool.
    pub next_player_max_juice: u8,
}

/// A resolved Cop Event, carried by [`Event::CopEventTriggered`] and thus by the
/// emitted `cop.event.triggered` event. Reaching the upper Heat bound triggers a
/// Cop Event; resolving the seeded d10 draw applies its effect and resets the
/// seat's Heat per the rules-contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopEventTriggered {
    /// The match the Cop Event fired in.
    pub match_id: String,
    /// The player identity whose Heat triggered the Cop Event.
    pub player_id: String,
    /// The seat that resolved the Cop Event.
    pub player: Player,
    /// The seeded d10 draw that selected the Cop Event from the table
    /// (`1..=`[`COP_EVENT_DIE_SIDES`]).
    pub rng_draw: u8,
    /// The seat's Heat after the Cop Event reset it (always within
    /// [`HEAT_BOUNDS`]; the rules-contract resets it to the lower bound).
    pub new_heat: i32,
}

/// A GainJuice card's Juice gain, carried by [`Event::JuiceGained`] and thus
/// by the emitted `juice.gained` event. Playing a `CardEffect::GainJuice` card
/// raises `available_juice`, capped at [`JUICE_CAP`]; this delta lets an
/// online client reconstruct that mutation instead of desyncing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JuiceGained {
    /// The match the Juice was gained in.
    pub match_id: String,
    /// The seat whose Juice rose.
    pub player: Player,
    /// The Juice gained by the card's declared amount (pre-cap).
    pub amount: u8,
    /// The seat's resulting available Juice after the gain (capped at
    /// [`JUICE_CAP`]).
    pub new_juice: u8,
}

/// A Cool effect's Heat reduction, carried by [`Event::HeatSet`] and thus by
/// the emitted `heat.set` event. Both `CardEffect::Cool` and
/// `HeroPowerEffect::Cool` lower `starting_heat`, floored at
/// [`HEAT_BOUNDS`]'s start; this delta lets an online client reconstruct that
/// mutation instead of desyncing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeatSet {
    /// The match the Heat was lowered in.
    pub match_id: String,
    /// The seat whose Heat was lowered.
    pub player: Player,
    /// The seat's resulting Heat after the reduction (floored at
    /// [`HEAT_BOUNDS`]'s start).
    pub new_heat: i32,
}

/// A GainArmor hero power's Boss HP gain, carried by
/// [`Event::BossArmorGained`] and thus by the emitted `boss.armor.gained`
/// event. `HeroPowerEffect::GainArmor` raises the activating seat's own
/// `boss_hp`; this delta lets an online client reconstruct that mutation
/// instead of desyncing. (The client fold for `boss.armor.gained` is added in
/// a follow-up task — the engine is the authority here.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BossArmorGained {
    /// The match the armor was gained in.
    pub match_id: String,
    /// The seat whose Boss gained HP.
    pub player: Player,
    /// The HP gained by the hero power's declared amount.
    pub amount: u8,
    /// The Boss's resulting HP after the gain.
    pub new_hp: i32,
}

/// A conceded match, carried by [`Event::MatchCompleted`] and thus by the
/// emitted `match.completed` event. A concede forfeits for one seat, so the
/// match ends yielding exactly one winner — the opposing seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchCompleted {
    /// The match that was completed by the concession.
    pub match_id: String,
    /// The player identity that conceded (forfeited) the match.
    pub conceding_player_id: String,
    /// The seat that conceded.
    pub conceding_player: Player,
    /// The player identity awarded the win.
    pub winning_player_id: String,
    /// The winning seat — the opponent of the conceding seat, so exactly one
    /// winner is produced.
    pub winner: Player,
}

/// A resolved venue event (Task 10, City-pillar hook), carried by
/// [`Event::VenueEventResolved`] and thus by the emitted
/// `venue.event.resolved` event. Subsystem 1's venue event table is a single
/// no-op entry — the draw selects it and changes nothing; Subsystem 3 grows
/// the table with real venue events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueEventResolved {
    /// The match the venue event fired in.
    pub match_id: String,
    /// Reference into the venue's event table the draw resolved against.
    pub event_table_ref: String,
    /// The seeded RNG draw that selected the entry from the venue event
    /// table.
    pub rng_draw: u8,
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
    /// A board unit took combat damage.
    OperatorDamaged(OperatorDamaged),
    /// A board unit was destroyed by combat.
    OperatorDied(OperatorDied),
    /// A Boss took combat damage.
    BossDamaged(BossDamaged),
    /// A board unit spent its attack and is exhausted for the turn.
    OperatorExhausted(OperatorExhausted),
    /// A Summon effect put an unready unit on the acting seat's board.
    OperatorSummoned(OperatorSummoned),
    /// A Boss trademark hero power passed every invariant, was paid for, and
    /// was activated.
    HeroPowerActivated(HeroPowerActivated),
    /// Ending a turn resolved the incoming seat's start-of-turn draw.
    FatigueDamageDealt(FatigueDamageDealt),
    /// The incoming seat's board units were readied at its turn start.
    OperatorsReadied(OperatorsReadied),
    /// The turn passed from the ending seat to its opponent.
    TurnEnded(TurnEnded),
    /// A Cop Event (fired when Heat hit the upper bound) was resolved from the
    /// seeded d10 table, resetting the seat's Heat.
    CopEventTriggered(CopEventTriggered),
    /// A player conceded, forfeiting the match to the opposing seat.
    MatchCompleted(MatchCompleted),
    /// A GainJuice card raised the acting player's available Juice.
    JuiceGained(JuiceGained),
    /// A Cool effect (card or hero power) lowered a seat's Heat.
    HeatSet(HeatSet),
    /// A GainArmor hero power raised the activating seat's own Boss HP.
    BossArmorGained(BossArmorGained),
    /// A venue event (Task 10, City-pillar hook) was resolved from the
    /// seeded venue event table.
    VenueEventResolved(VenueEventResolved),
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
            Event::OperatorDamaged(_) => "operator.damaged",
            Event::OperatorDied(_) => "operator.died",
            Event::BossDamaged(_) => "boss.damaged",
            Event::OperatorExhausted(_) => "operator.exhausted",
            Event::OperatorSummoned(_) => "operator.summoned",
            Event::HeroPowerActivated(_) => "hero_power.activated",
            Event::FatigueDamageDealt(_) => "fatigue.damage.dealt",
            Event::OperatorsReadied(_) => "operators.readied",
            Event::TurnEnded(_) => "turn.ended",
            Event::CopEventTriggered(_) => "cop.event.triggered",
            Event::MatchCompleted(_) => "match.completed",
            Event::JuiceGained(_) => "juice.gained",
            Event::HeatSet(_) => "heat.set",
            Event::BossArmorGained(_) => "boss.armor.gained",
            Event::VenueEventResolved(_) => "venue.event.resolved",
        }
    }
}

/// The closed set of card effects the engine can resolve. Mirrors the client's
/// `resolveEffect` (web/src/match/rules.ts:299). Extended in Subsystem 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CardEffect {
    None,
    DealDamage { amount: u8 },
    Summon, // stats come from the CardInstance's atk/hp
    DrawCards { amount: u8 },
    GainJuice { amount: u8 },
    Cool { amount: u8 }, // lower own Heat
}

impl CardEffect {
    /// Total mapping from a catalog `effect_script_ref` to a resolvable effect.
    /// `amount` fields default to 0 here; the concrete amount is carried on the
    /// CardInstance at deck-build (Task 4). Returns None for unregistered names.
    pub fn from_script_ref(script_ref: &str) -> Option<CardEffect> {
        Some(match script_ref {
            "effect.noop" => CardEffect::None,
            "effect.deal_damage" => CardEffect::DealDamage { amount: 0 },
            "effect.draw_card" => CardEffect::DrawCards { amount: 0 },
            "effect.gain_juice" => CardEffect::GainJuice { amount: 0 },
            "effect.cool" => CardEffect::Cool { amount: 0 },
            "effect.recruit_operator" => CardEffect::Summon,
            // Subsystem-2 mechanics: registered + validated, resolve to no-op for now.
            "effect.steal_piece" | "effect.pull_heist" => CardEffect::None,
            _ => return None,
        })
    }
}

/// Engine-semantic keywords (bound to real behavior in combat/summon), not inert
/// strings. Mirrors the client's ad-hoc Spotlight/Drive-By checks. Extended in
/// Subsystem 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Keyword {
    Spotlight, // taunt: enemy attacks must target a Spotlight unit first
    DriveBy,   // on arrival, deal damage to the enemy Boss
}

impl Keyword {
    /// Parse a catalog keyword string; unknown keywords are rejected (mirrors
    /// CardType::parse). Accepts the client's exact spellings.
    pub fn parse(raw: &str) -> Result<Keyword, DomainError> {
        match raw {
            "Spotlight" => Ok(Keyword::Spotlight),
            "Drive-By" => Ok(Keyword::DriveBy),
            other => Err(DomainError::InvariantViolation(format!(
                "unknown keyword '{other}'"
            ))),
        }
    }
}

/// A card instance in a hand or deck: a definition ref + per-copy identity +
/// resolved play-stats. Populated from CardDefinition fields at deck-build.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CardInstance {
    pub instance_id: String, // e.g. "A-w_the_homie-3"
    pub card_id: String,     // definition id
    pub cost: u8,
    pub card_type: CardType, // Operator/Job/Piece/Vehicle/Heist
    pub effect: CardEffect,  // resolved effect + amount
    pub atk: u8,             // 0 for non-unit cards
    pub hp: u8,              // 0 for non-unit cards
    pub keywords: Vec<Keyword>,
    pub boss_lock: Option<String>, // Some(boss_id) if boss-locked (Task 8)
    /// The card's class allegiance (Task 10, City-pillar hook); defaults to
    /// [`CardClass::Neutral`] for the closed practice pool. Lets a
    /// [`LocationModifier`]'s `class_boosts` key off it downstream on the
    /// board via [`BoardUnit::class`].
    #[serde(default = "default_card_class")]
    pub class: CardClass,
}

/// A unit on the board (summoned Operator or Vehicle).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoardUnit {
    pub instance_id: String,
    pub card_id: String,
    pub atk: u8,
    pub hp: u8,
    pub max_hp: u8,
    pub ready: bool,      // false the turn it arrives (summoning sickness)
    pub is_vehicle: bool, // counts against MAX_VEHICLES vs MAX_OPERATORS
    pub keywords: Vec<Keyword>,
    /// The unit's class allegiance (Task 10, City-pillar hook), populated
    /// from the summoning [`CardInstance`]; defaults to
    /// [`CardClass::Neutral`]. The single key
    /// [`GameSession::apply_location_modifiers`] matches against.
    #[serde(default = "default_card_class")]
    pub class: CardClass,
}

/// Default class for a card/unit with no class data (the closed practice
/// pool, hero-power tokens): [`CardClass::Neutral`] never matches a
/// [`LocationModifier`]'s `class_boosts`, so it is always a safe default.
fn default_card_class() -> CardClass {
    CardClass::Neutral
}

/// Live per-seat state that the scalar OutfitConfig cannot express: the hand,
/// the ordered secret deck, and the board. Resource scalars (juice/heat/boss_hp)
/// stay on OutfitConfig for Subsystem 1 (see Task 4 design note).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeatState {
    pub hand: Vec<CardInstance>,
    pub deck: Vec<CardInstance>, // server-secret; ordered
    pub board: Vec<BoardUnit>,
}

/// The City pillar's neutral venue modifier (Task 10, the City-pillar hook):
/// a match is played at a venue whose modifiers affect BOTH seats alike. This
/// is the seam Subsystem 3 fills with real content (a venue catalog, event
/// tables, the growing map) — Subsystem 1 ships the plumbing only, so
/// [`GameSession`]'s `location` defaults `None` and every existing test stays
/// green (see [`GameSession::apply_location_modifiers`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LocationModifier {
    pub location_id: String,
    /// Data-driven venue kind, e.g. `"bank"` | `"chop_shop"`.
    pub location_type: String,
    /// Neutral atk boosts by class; applies to BOTH seats via
    /// [`GameSession::apply_location_modifiers`].
    pub class_boosts: Vec<(CardClass, i8)>,
    /// Multiplier applied to Heat gain at this venue (Subsystem 3 content;
    /// unused by Subsystem 1's plumbing). Default `1`.
    pub heat_multiplier: u8,
    /// Reference into the venue event table, drawn by [`ResolveVenueEvent`].
    pub event_table_ref: Option<String>,
}

/// Hand-rolled: `#[derive(Default)]` would give `heat_multiplier: 0`, silently
/// zeroing out Heat gain at any venue built via `..Default::default()`. The
/// documented/intended default is `1` (a no-op multiplier); every other field
/// keeps its natural zero/empty default.
impl Default for LocationModifier {
    fn default() -> Self {
        Self {
            location_id: String::new(),
            location_type: String::new(),
            class_boosts: vec![],
            heat_multiplier: 1,
            event_table_ref: None,
        }
    }
}

/// One entry in the closed practice card pool: a card definition's fixed
/// play-stats, ported from the client's `CARD_POOL` (web/src/match/rules.ts:50).
struct PoolCard {
    card_id: &'static str,
    cost: u8,
    card_type: CardType,
    effect: CardEffect,
    atk: u8,
    hp: u8,
    keywords: &'static [Keyword],
}

/// The 14-card pool a seeded 30-card deck is drawn from. A faithful port of the
/// client's `CARD_POOL` (web/src/match/rules.ts:50-65) — same ids, costs, types,
/// effects+amounts, stats, and keywords — so a Rust-dealt deck matches a
/// WASM-predicted one.
const CARD_POOL: &[PoolCard] = &[
    PoolCard {
        card_id: "bolt",
        cost: 1,
        card_type: CardType::Job,
        effect: CardEffect::DealDamage { amount: 3 },
        atk: 0,
        hp: 0,
        keywords: &[],
    },
    PoolCard {
        card_id: "w_corner_boy",
        cost: 1,
        card_type: CardType::Operator,
        effect: CardEffect::Summon,
        atk: 1,
        hp: 2,
        keywords: &[],
    },
    PoolCard {
        card_id: "pd_beat_cop",
        cost: 1,
        card_type: CardType::Operator,
        effect: CardEffect::Summon,
        atk: 1,
        hp: 2,
        keywords: &[],
    },
    PoolCard {
        card_id: "w_young_buck",
        cost: 1,
        card_type: CardType::Operator,
        effect: CardEffect::Summon,
        atk: 2,
        hp: 1,
        keywords: &[],
    },
    PoolCard {
        card_id: "w_drive_by",
        cost: 2,
        card_type: CardType::Job,
        effect: CardEffect::DealDamage { amount: 4 },
        atk: 0,
        hp: 0,
        keywords: &[],
    },
    PoolCard {
        card_id: "w_the_homie",
        cost: 2,
        card_type: CardType::Operator,
        effect: CardEffect::Summon,
        atk: 3,
        hp: 2,
        keywords: &[],
    },
    PoolCard {
        card_id: "w_the_enforcer",
        cost: 3,
        card_type: CardType::Operator,
        effect: CardEffect::Summon,
        atk: 2,
        hp: 5,
        keywords: &[Keyword::Spotlight],
    },
    PoolCard {
        card_id: "pd_riot_squad",
        cost: 5,
        card_type: CardType::Operator,
        effect: CardEffect::Summon,
        atk: 4,
        hp: 5,
        keywords: &[Keyword::Spotlight],
    },
    PoolCard {
        card_id: "pd_the_crib",
        cost: 2,
        card_type: CardType::Piece,
        effect: CardEffect::Cool { amount: 2 },
        atk: 0,
        hp: 0,
        keywords: &[],
    },
    PoolCard {
        card_id: "ht_the_come_up",
        cost: 2,
        card_type: CardType::Piece,
        effect: CardEffect::GainJuice { amount: 2 },
        atk: 0,
        hp: 0,
        keywords: &[],
    },
    PoolCard {
        card_id: "w_stolen_whip",
        cost: 3,
        card_type: CardType::Vehicle,
        effect: CardEffect::Summon,
        atk: 4,
        hp: 3,
        keywords: &[Keyword::DriveBy],
    },
    PoolCard {
        card_id: "w_blow_the_safe",
        cost: 3,
        card_type: CardType::Job,
        effect: CardEffect::DrawCards { amount: 2 },
        atk: 0,
        hp: 0,
        keywords: &[],
    },
    PoolCard {
        card_id: "w_shot_caller",
        cost: 4,
        card_type: CardType::Operator,
        effect: CardEffect::Summon,
        atk: 5,
        hp: 5,
        keywords: &[],
    },
    PoolCard {
        card_id: "w_the_big_one",
        cost: 5,
        card_type: CardType::Heist,
        effect: CardEffect::DealDamage { amount: 7 },
        atk: 0,
        hp: 0,
        keywords: &[],
    },
];

/// The client's mulberry32 PRNG (web/src/match/rules.ts:71), reproduced exactly
/// so a Rust-dealt deck matches a WASM-predicted one bit-for-bit. JS `Math.imul`
/// is u32 `wrapping_mul`, JS `>>>` is `>>` on `u32`, and the result is already a
/// `u32` (no `>>> 0` coercion needed).
fn mulberry32(mut state: u32) -> impl FnMut() -> f64 {
    move || {
        state = state.wrapping_add(0x6D2B_79F5);
        let mut t = state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        ((t ^ (t >> 14)) as f64) / 4_294_967_296.0
    }
}

/// Render a seat as the single-character label the client uses in instance ids.
fn seat_label(seat: Player) -> char {
    match seat {
        Player::A => 'A',
        Player::B => 'B',
    }
}

/// Build a shuffled 30-card deck of instanced cards for `seat`, seeded. A port of
/// the client's `buildDeck` (web/src/match/rules.ts:82): the seat's stream is
/// `seed ^ 0x1111` (A) / `seed ^ 0x2222` (B); 30 cards are drawn from the pool
/// then Fisher–Yates shuffled with the same stream, so instance ids and order
/// match the client bit-for-bit.
fn build_deck(seed: u64, seat: Player) -> Vec<CardInstance> {
    let seat_salt: u32 = match seat {
        Player::A => 0x1111,
        Player::B => 0x2222,
    };
    let mut rng = mulberry32((seed as u32) ^ seat_salt);
    let pool_len = CARD_POOL.len();
    let mut cards: Vec<CardInstance> = Vec::with_capacity(30);
    let mut n = 0usize;
    while cards.len() < 30 {
        let idx = (rng() * pool_len as f64).floor() as usize;
        let def = &CARD_POOL[idx];
        cards.push(CardInstance {
            instance_id: format!("{}-{}-{}", seat_label(seat), def.card_id, n),
            card_id: def.card_id.to_string(),
            cost: def.cost,
            card_type: def.card_type,
            effect: def.effect,
            atk: def.atk,
            hp: def.hp,
            keywords: def.keywords.to_vec(),
            boss_lock: None,
            class: CardClass::Neutral,
        });
        n += 1;
    }
    // Fisher–Yates with the same seeded stream (mirrors the client's loop).
    for i in (1..cards.len()).rev() {
        let j = (rng() * (i as f64 + 1.0)).floor() as usize;
        cards.swap(i, j);
    }
    cards
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
    /// Live per-seat state for player `A` (hand/deck/board). Starts empty and is
    /// populated by [`GameSession::start_match`]; coexists with [`OutfitConfig`],
    /// which remains the opening input and the home of the resource scalars.
    seat_a: SeatState,
    /// Live per-seat state for player `B` (hand/deck/board).
    seat_b: SeatState,
    /// The venue this match is played at (Task 10, City-pillar hook). `None`
    /// (the default) makes [`GameSession::apply_location_modifiers`] an
    /// identity — the seam Subsystem 3 fills with real venue content.
    location: Option<LocationModifier>,
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
            seat_a: SeatState::default(),
            seat_b: SeatState::default(),
            location: None,
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

    /// Set the venue this match is played at (Task 10, City-pillar hook).
    /// Configuring `None` (the default) restores identity behavior in
    /// [`GameSession::apply_location_modifiers`].
    pub fn set_location(&mut self, location: LocationModifier) {
        self.location = Some(location);
    }

    /// The ONE place location modifiers touch unit stats. Identity when no
    /// location is set; otherwise applies neutral class boosts (both seats).
    fn apply_location_modifiers(&self, _seat: Player, base: &BoardUnit) -> BoardUnit {
        let mut out = base.clone();
        if let Some(loc) = &self.location {
            for (class, delta) in &loc.class_boosts {
                if *class == out.class {
                    out.atk = (out.atk as i16 + *delta as i16).max(0) as u8;
                }
            }
        }
        out
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
            if outfit.max_juice > JUICE_CAP {
                return Err(DomainError::InvariantViolation(format!(
                    "player {seat:?} Outfit '{}' has max Juice {}, exceeding the hard cap of {JUICE_CAP}",
                    outfit.name, outfit.max_juice
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

    /// Boss-lock invariant (Task 8, server-authoritative anti-cheat backstop):
    /// every card dealt into a seat's deck or hand that is locked to a Boss
    /// (`CardInstance.boss_lock`) must be locked to *that seat's own* Boss.
    /// Since a deck backs a tradeable asset, this is re-checked here even
    /// though the Outfit aggregate already enforces the same rule at
    /// deck-build time — a client cannot be trusted to have gone through it.
    ///
    /// Takes `seat`/`boss`/`cards` as plain values rather than reading
    /// `self.seat_a`/`self.seat_b` so callers can validate a *locally built*
    /// deal before committing it to seat state — a rejected command must
    /// mutate nothing (see `start_match`).
    fn ensure_boss_locks_honored<'a>(
        seat: Player,
        boss: &str,
        cards: impl Iterator<Item = &'a CardInstance>,
    ) -> Result<(), DomainError> {
        for c in cards {
            if let Some(b) = &c.boss_lock {
                if b != boss {
                    return Err(DomainError::InvariantViolation(format!(
                        "seat {seat:?} decks card '{}' locked to Boss '{b}', but its Boss is '{boss}'",
                        c.card_id
                    )));
                }
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

        // Deal both seats from the seeded 30-card deck into LOCAL variables
        // first: the opening hand is the first OPENING_HAND cards, the rest
        // stays as the ordered secret deck. build_deck ports the client's
        // mulberry32/buildDeck so a WASM-predicted deal matches this one
        // bit-for-bit.
        //
        // The boss-lock backstop (Task 8, server-authoritative anti-cheat:
        // decks back tradeable assets) is validated against these locals
        // BEFORE any write to `self.seat_a`/`self.seat_b` — a rejected
        // command must mutate nothing, and this session is a long-lived
        // in-memory aggregate (see `crates/server/src/ws/hub.rs`), not
        // discarded on `Err`.
        let seed = cmd.rng_seed;
        let mut dealt: Vec<(Player, Vec<CardInstance>, Vec<CardInstance>)> = Vec::new();
        for seat in [Player::A, Player::B] {
            let mut deck = build_deck(seed, seat);
            let hand: Vec<CardInstance> = deck.drain(0..OPENING_HAND.min(deck.len())).collect();
            let boss = self.outfit_at(seat).boss_name.clone();
            Self::ensure_boss_locks_honored(seat, &boss, deck.iter().chain(hand.iter()))?;
            dealt.push((seat, deck, hand));
        }

        // Both seats validated cleanly: only now is it safe to commit the
        // deal to live seat state.
        for (seat, deck, hand) in dealt {
            let st = self.seat_state_at_mut(seat);
            st.hand = hand;
            st.deck = deck;
            st.board = Vec::new();
        }

        // Thread the command's venue (Task 10, Global Constraint: a match
        // must be startable AT a venue via the command path, not only via
        // the server-side `set_location` config method) into live state.
        // `None` (an existing StartMatchCmd payload with no `location`)
        // leaves `apply_location_modifiers` an identity, same as before.
        self.location = cmd.location;

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

    /// The live [`SeatState`] (hand/deck/board) seated at `seat`.
    pub fn seat_state_at(&self, seat: Player) -> &SeatState {
        match seat {
            Player::A => &self.seat_a,
            Player::B => &self.seat_b,
        }
    }

    /// Mutable access to the live [`SeatState`] seated at `seat`.
    pub fn seat_state_at_mut(&mut self, seat: Player) -> &mut SeatState {
        match seat {
            Player::A => &mut self.seat_a,
            Player::B => &mut self.seat_b,
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

        // Find the played instance in the acting seat's hand; its cost is
        // authoritative. Absent → the card is not in that hand.
        let instance = self
            .seat_state_at(seat)
            .hand
            .iter()
            .find(|c| c.instance_id == cmd.card_instance_id)
            .cloned()
            .ok_or_else(|| {
                DomainError::InvariantViolation(format!(
                    "card '{}' is not in {seat:?}'s hand",
                    cmd.card_instance_id
                ))
            })?;
        // Anti-cheat: the client cannot understate (or overstate) the cost.
        if cmd.juice_cost != instance.cost {
            return Err(DomainError::InvariantViolation(format!(
                "declared cost {} does not match card cost {}",
                cmd.juice_cost, instance.cost
            )));
        }
        // Board-cap pre-check for summons, before mutating anything (a rejected
        // play must leave state untouched).
        if matches!(instance.effect, CardEffect::Summon) {
            self.ensure_summon_capacity(seat, instance.card_type)?;
        }

        // Pay the card's Juice cost, and compute the Heat the play raises.
        self.ensure_card_affordable(seat, cmd.juice_cost)?;
        let new_heat = self.heat_after_play(seat)?;

        // Mutate state: deduct the Juice and persist the Heat raise. (Previously
        // play_card only emitted these deltas without applying them — the bug.)
        {
            let outfit = self.outfit_at_mut(seat);
            outfit.available_juice -= cmd.juice_cost;
            outfit.starting_heat = new_heat;
        }

        // The card's target, kept for effect resolution after the command's
        // owned fields are moved into the CardPlayed event below.
        let target_ref = cmd.target_ref.clone();

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

        // Remove the played card from hand, then resolve its effect against
        // state, recording each effect delta after CardPlayed/HeatRaised.
        self.seat_state_at_mut(seat)
            .hand
            .retain(|c| c.instance_id != instance.instance_id);
        let mut effect_events = self.resolve_effect(seat, &instance, &target_ref);
        for e in &effect_events {
            self.root.record(Box::new(e.clone()));
        }
        let mut all = vec![played, raised];
        all.append(&mut effect_events);
        Ok(all)
    }

    /// Summon-capacity invariant: a seat's board may hold at most
    /// [`MAX_OPERATORS`] Operators and [`MAX_VEHICLES`] Vehicles simultaneously.
    /// Checked against the *live* board before a Summon mutates it, so a rejected
    /// summon leaves state untouched.
    fn ensure_summon_capacity(&self, seat: Player, card_type: CardType) -> Result<(), DomainError> {
        let is_vehicle = matches!(card_type, CardType::Vehicle);
        let count = self
            .seat_state_at(seat)
            .board
            .iter()
            .filter(|u| u.is_vehicle == is_vehicle)
            .count();
        let cap = if is_vehicle {
            MAX_VEHICLES
        } else {
            MAX_OPERATORS
        };
        if count >= cap {
            return Err(DomainError::InvariantViolation(format!(
                "{seat:?}'s board is at the {} cap of {cap}",
                if is_vehicle { "Vehicle" } else { "Operator" }
            )));
        }
        Ok(())
    }

    /// Reduce `foe`'s Boss HP by `amount` (clamped at 0), emitting
    /// [`Event::BossDamaged`] and, at 0, [`Event::BossDefeated`] (winner is
    /// `foe`'s opponent). Mirrors the terminal Boss handling in `declare_attack`.
    fn damage_boss(&mut self, foe: Player, amount: i32) -> Vec<Event> {
        let mid = self.match_id.clone();
        let new_hp = {
            let outfit = self.outfit_at_mut(foe);
            outfit.boss_hp -= amount;
            let clamped = outfit.boss_hp.max(0);
            outfit.boss_hp = clamped;
            clamped
        };
        let mut out = vec![Event::BossDamaged(BossDamaged {
            match_id: mid.clone(),
            player: foe,
            amount,
            new_hp,
        })];
        if new_hp == 0 {
            out.push(Event::BossDefeated(BossDefeated {
                match_id: mid,
                defeated_player_id: self.outfit_at(foe).name.clone(),
                defeated_player: foe,
                boss_id: self.outfit_at(foe).boss_name.clone(),
                winner: Self::opponent_of(foe),
            }));
        }
        out
    }

    /// Resolve a `DealDamage` target reference: `"op:<instance_id>"` damages an
    /// enemy board unit (reusing [`apply_unit_damage`](Self::apply_unit_damage)),
    /// anything else (`"boss:<seat>"`) damages the enemy Boss.
    fn damage_target(&mut self, target_ref: &str, amount: u8, foe: Player) -> Vec<Event> {
        if let Some(id) = target_ref.strip_prefix("op:") {
            self.apply_unit_damage(foe, id, amount)
        } else {
            self.damage_boss(foe, amount as i32)
        }
    }

    /// Draw the top card of `seat`'s ordered deck into its hand, returning it.
    /// `None` when the deck is empty (a real draw would deal Fatigue instead;
    /// modeled by the start-of-turn draw, not by card effects).
    fn draw_one(&mut self, seat: Player) -> Option<CardInstance> {
        let st = self.seat_state_at_mut(seat);
        if st.deck.is_empty() {
            return None;
        }
        let card = st.deck.remove(0);
        st.hand.push(card.clone());
        Some(card)
    }

    /// Port of the client's `resolveEffect` (web/src/match/rules.ts:299): mutate
    /// state for `card`'s [`CardEffect`] and return the deltas the client folds.
    /// Summon puts an unready [`BoardUnit`] on `seat`'s board (firing Drive-By at
    /// the enemy Boss on arrival); DealDamage hits the target; GainJuice/Cool
    /// adjust resources; DrawCards pulls from the deck.
    fn resolve_effect(
        &mut self,
        seat: Player,
        card: &CardInstance,
        target_ref: &str,
    ) -> Vec<Event> {
        let foe = Self::opponent_of(seat);
        let mid = self.match_id.clone();
        let mut out = Vec::new();
        match card.effect {
            CardEffect::None => {}
            CardEffect::DealDamage { amount } => {
                let tref = if target_ref.is_empty() {
                    format!("boss:{foe:?}")
                } else {
                    target_ref.to_string()
                };
                out.extend(self.damage_target(&tref, amount, foe));
            }
            CardEffect::Summon => {
                let unit = BoardUnit {
                    instance_id: card.instance_id.clone(),
                    card_id: card.card_id.clone(),
                    atk: card.atk,
                    hp: card.hp,
                    max_hp: card.hp,
                    ready: false,
                    is_vehicle: matches!(card.card_type, CardType::Vehicle),
                    keywords: card.keywords.clone(),
                    class: card.class,
                };
                self.seat_state_at_mut(seat).board.push(unit.clone());
                out.push(Event::OperatorSummoned(OperatorSummoned {
                    match_id: mid,
                    player: seat,
                    unit,
                }));
                // Drive-By: strafe the enemy Boss on arrival.
                if card.keywords.contains(&Keyword::DriveBy) {
                    out.extend(self.damage_boss(foe, DRIVE_BY_DAMAGE as i32));
                }
            }
            CardEffect::DrawCards { amount } => {
                for _ in 0..amount {
                    let _ = self.draw_one(seat);
                }
            }
            CardEffect::GainJuice { amount } => {
                let o = self.outfit_at_mut(seat);
                o.available_juice = o.available_juice.saturating_add(amount).min(JUICE_CAP);
                let new_juice = o.available_juice;
                out.push(Event::JuiceGained(JuiceGained {
                    match_id: mid,
                    player: seat,
                    amount,
                    new_juice,
                }));
            }
            CardEffect::Cool { amount } => {
                let o = self.outfit_at_mut(seat);
                o.starting_heat = (o.starting_heat - amount as i32).max(*HEAT_BOUNDS.start());
                let new_heat = o.starting_heat;
                out.push(Event::HeatSet(HeatSet {
                    match_id: mid,
                    player: seat,
                    new_heat,
                }));
            }
        }
        out
    }

    /// Handle `AttackCmd`: verify the command targets this match and a real
    /// turn-holding player; enforce every match-play invariant; then resolve
    /// real combat over [`BoardUnit`]s. The attacker deals its atk to the
    /// target and, if the target is a unit, takes simultaneous retaliation;
    /// dead units are removed and the surviving attacker exhausts. A Boss
    /// target reduces `boss_hp`, emitting [`Event::BossDamaged`] and, at 0,
    /// [`Event::BossDefeated`].
    fn declare_attack(&mut self, cmd: Attack) -> Result<Vec<Event>, DomainError> {
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

        // An attacker must be named; its existence/readiness is checked below
        // against the live board.
        if cmd.attacker_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "an attackerId must be provided".to_string(),
            ));
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

        // Resolve target from target_ref: "boss:<seat>" | "op:<instance_id>".
        let defending_player = Self::opponent_of(seat);
        let target = cmd.target_ref.as_str();

        // Attacker must be a READY unit the acting seat owns.
        let attacker = self
            .seat_state_at(seat)
            .board
            .iter()
            .find(|u| u.instance_id == cmd.attacker_id)
            .ok_or_else(|| {
                DomainError::InvariantViolation(format!(
                    "no unit '{}' on the attacker's board",
                    cmd.attacker_id
                ))
            })?;
        if !attacker.ready {
            return Err(DomainError::InvariantViolation(format!(
                "unit '{}' is not ready (summoning sickness)",
                cmd.attacker_id
            )));
        }
        // Consult the location-modifier seam (Task 10, City-pillar hook) at the
        // ONE place combat reads the attacker's atk. Identity when no location
        // is set (Subsystem 1's default); Subsystem 3's venue content boosts
        // matching classes for both seats here without touching this call site.
        let attacker_atk = self.apply_location_modifiers(seat, attacker).atk;

        // Spotlight: if the defender has any Spotlight unit, the target must be one.
        let has_spotlight = self
            .seat_state_at(defending_player)
            .board
            .iter()
            .any(|u| u.keywords.contains(&Keyword::Spotlight));
        if has_spotlight {
            let targeting_spotlight = target.strip_prefix("op:").is_some_and(|id| {
                self.seat_state_at(defending_player)
                    .board
                    .iter()
                    .any(|u| u.instance_id == id && u.keywords.contains(&Keyword::Spotlight))
            });
            if !targeting_spotlight {
                return Err(DomainError::InvariantViolation(
                    "must attack a Spotlight unit first".to_string(),
                ));
            }
        }

        let mut events: Vec<Event> = Vec::new();
        if let Some(defender_id) = target.strip_prefix("op:") {
            // Capture both attack values BEFORE applying damage (simultaneous).
            let retaliation = self
                .seat_state_at(defending_player)
                .board
                .iter()
                .find(|u| u.instance_id == defender_id)
                .map(|u| u.atk)
                .ok_or_else(|| {
                    DomainError::InvariantViolation(format!("no defender '{defender_id}'"))
                })?;
            // Apply attacker -> defender.
            events.extend(self.apply_unit_damage(defending_player, defender_id, attacker_atk));
            // Apply defender -> attacker (retaliation).
            if retaliation > 0 {
                events.extend(self.apply_unit_damage(seat, &cmd.attacker_id, retaliation));
            }
        } else if let Some(boss_seat) = target.strip_prefix("boss:") {
            let _ = boss_seat; // target names the enemy boss; enforce it is the defender
            let outfit = self.outfit_at_mut(defending_player);
            outfit.boss_hp -= attacker_atk as i32;
            let new_hp = outfit.boss_hp.max(0);
            events.push(Event::BossDamaged(BossDamaged {
                match_id: self.match_id.clone(),
                player: defending_player,
                amount: attacker_atk as i32,
                new_hp,
            }));
            if new_hp == 0 {
                // Terminal: reuse the existing BossDefeated event (what the old
                // declare_attack emitted) — MatchCompleted is concession-shaped
                // and wrong for a combat kill.
                let defeated_player_id = self.outfit_at(defending_player).name.clone();
                let boss_id = self.outfit_at(defending_player).boss_name.clone();
                events.push(Event::BossDefeated(BossDefeated {
                    match_id: self.match_id.clone(),
                    defeated_player_id,
                    defeated_player: defending_player,
                    boss_id,
                    winner: seat,
                }));
            }
        } else {
            return Err(DomainError::InvariantViolation(format!(
                "malformed targetRef '{}'",
                cmd.target_ref
            )));
        }

        // Attacker exhausts if it survived the trade.
        if self
            .seat_state_at(seat)
            .board
            .iter()
            .any(|u| u.instance_id == cmd.attacker_id)
        {
            events.push(Event::OperatorExhausted(OperatorExhausted {
                match_id: self.match_id.clone(),
                player: seat,
                instance_id: cmd.attacker_id.clone(),
            }));
            if let Some(u) = self
                .seat_state_at_mut(seat)
                .board
                .iter_mut()
                .find(|u| u.instance_id == cmd.attacker_id)
            {
                u.ready = false;
            }
        }

        for e in &events {
            self.root.record(Box::new(e.clone()));
        }
        Ok(events)
    }

    /// Apply `amount` damage to `owner`'s unit `instance_id`, returning the
    /// resulting deltas ([`Event::OperatorDamaged`], and [`Event::OperatorDied`]
    /// if it drops to 0). Removes the unit from the board when it dies. Reused
    /// by effect resolution (Task 6).
    fn apply_unit_damage(&mut self, owner: Player, instance_id: &str, amount: u8) -> Vec<Event> {
        let mut out = Vec::new();
        let board = &mut self.seat_state_at_mut(owner).board;
        if let Some(u) = board.iter_mut().find(|u| u.instance_id == instance_id) {
            let new_hp = u.hp.saturating_sub(amount);
            u.hp = new_hp;
            out.push(Event::OperatorDamaged(OperatorDamaged {
                match_id: String::new(),
                player: owner,
                instance_id: instance_id.to_string(),
                new_hp,
            }));
            if new_hp == 0 {
                out.push(Event::OperatorDied(OperatorDied {
                    match_id: String::new(),
                    player: owner,
                    instance_id: instance_id.to_string(),
                }));
            }
        }
        if let Some(dead) = out.iter().find_map(|e| {
            if let Event::OperatorDied(d) = e {
                Some(d.instance_id.clone())
            } else {
                None
            }
        }) {
            self.seat_state_at_mut(owner)
                .board
                .retain(|b| b.instance_id != dead);
        }
        // Fill in match_id after the &mut board borrow has ended.
        let mid = self.match_id.clone();
        for e in out.iter_mut() {
            match e {
                Event::OperatorDamaged(d) => d.match_id = mid.clone(),
                Event::OperatorDied(d) => d.match_id = mid.clone(),
                _ => {}
            }
        }
        out
    }

    /// Handle `ActivateHeroPowerCmd`: verify the command targets this match, a
    /// real player whose turn it is, and a well-formed target; enforce every
    /// match-play invariant against the session's state; pay the Boss hero
    /// power's Juice cost; and emit [`Event::HeroPowerActivated`].
    fn activate_hero_power(&mut self, cmd: ActivateHeroPower) -> Result<Vec<Event>, DomainError> {
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

        // The hero power's target must be identified.
        if cmd.target_ref.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "a targetRef must be provided".to_string(),
            ));
        }

        // Enforce every match-play invariant before applying the activation.
        self.ensure_boards_within_caps()?;
        self.ensure_heat_within_bounds()?;
        self.ensure_starting_juice_valid()?;
        self.ensure_decks_nonempty()?;
        self.ensure_heists_prereqs_satisfied()?;
        self.ensure_bosses_alive()?;
        let turn_player = self.ensure_opening_player_designated()?;

        // Turn-ownership: a hero power is activated only by the player whose
        // turn it is.
        if seat != turn_player {
            return Err(DomainError::InvariantViolation(format!(
                "player '{}' (seat {seat:?}) may not activate a hero power; it is player {turn_player:?}'s turn",
                cmd.player_id
            )));
        }

        // Anti-cheat: the command's claimed cost must equal the Boss's declared
        // hero_power_cost — a client cannot understate (or overstate) it.
        let declared_cost = self.outfit_at(seat).hero_power_cost;
        if cmd.juice_cost != declared_cost {
            return Err(DomainError::InvariantViolation(format!(
                "hero power costs {declared_cost} Juice but the command claims {}",
                cmd.juice_cost
            )));
        }

        // The Boss hero power is paid for out of the seat's available Juice; it
        // may only be activated when its cost does not exceed that pool.
        self.ensure_card_affordable(seat, cmd.juice_cost)?;

        // Pay the cost, deducting it from the seat's available Juice. The
        // affordability check guarantees this cannot underflow, and the result
        // stays within the Juice hard cap.
        let outfit = self.outfit_at_mut(seat);
        outfit.available_juice -= cmd.juice_cost;
        let remaining_juice = outfit.available_juice;

        let target_ref = cmd.target_ref.clone();
        let activated = Event::HeroPowerActivated(HeroPowerActivated {
            match_id: cmd.match_id,
            player_id: cmd.player_id,
            player: seat,
            target_ref: cmd.target_ref,
            juice_spent: cmd.juice_cost,
            remaining_juice,
        });
        self.root.record(Box::new(activated.clone()));

        // Resolve the Boss's declared hero-power effect against the target,
        // emitting its deltas (boss.damaged, operator.summoned, ...) after
        // HeroPowerActivated.
        let effect = self.outfit_at(seat).hero_power_effect;
        let mut effect_events = self.resolve_hero_power(seat, effect, &target_ref);
        for e in &effect_events {
            self.root.record(Box::new(e.clone()));
        }
        let mut all = vec![activated];
        all.append(&mut effect_events);
        Ok(all)
    }

    /// Resolve a Boss's [`HeroPowerEffect`] for the activating `seat` against
    /// `target_ref`, mirroring [`GameSession::resolve_effect`]'s shape (and
    /// reusing its Task-6 helpers) but for hero powers. `DealDamage` hits the
    /// target (reusing [`GameSession::damage_target`]); `GainArmor` raises the
    /// activating seat's own Boss HP; `Cool` lowers the activating seat's own
    /// Heat, floored at [`HEAT_BOUNDS`]'s start; `SummonToken` puts an unready
    /// token [`BoardUnit`] on the activating seat's board, respecting
    /// [`GameSession::ensure_summon_capacity`] — a full board simply skips the
    /// summon (mirrors Hearthstone: a hero power never fizzles the whole
    /// activation just because the board is full).
    fn resolve_hero_power(
        &mut self,
        seat: Player,
        effect: HeroPowerEffect,
        target_ref: &str,
    ) -> Vec<Event> {
        let foe = Self::opponent_of(seat);
        match effect {
            HeroPowerEffect::DealDamage { amount } => {
                let tref = if target_ref.is_empty() {
                    format!("boss:{foe:?}")
                } else {
                    target_ref.to_string()
                };
                self.damage_target(&tref, amount, foe)
            }
            HeroPowerEffect::GainArmor { amount } => {
                let outfit = self.outfit_at_mut(seat);
                outfit.boss_hp += amount as i32;
                let new_hp = outfit.boss_hp;
                let mid = self.match_id.clone();
                vec![Event::BossArmorGained(BossArmorGained {
                    match_id: mid,
                    player: seat,
                    amount,
                    new_hp,
                })]
            }
            HeroPowerEffect::Cool { amount } => {
                let outfit = self.outfit_at_mut(seat);
                outfit.starting_heat =
                    (outfit.starting_heat - amount as i32).max(*HEAT_BOUNDS.start());
                let new_heat = outfit.starting_heat;
                let mid = self.match_id.clone();
                vec![Event::HeatSet(HeatSet {
                    match_id: mid,
                    player: seat,
                    new_heat,
                })]
            }
            HeroPowerEffect::SummonToken { atk, hp } => {
                // A full board is not a rejection — the token is just not
                // summoned, mirroring Hearthstone hero powers.
                if self
                    .ensure_summon_capacity(seat, CardType::Operator)
                    .is_err()
                {
                    return Vec::new();
                }
                let instance_id = format!(
                    "{}-hero-token-{}",
                    seat_label(seat),
                    self.seat_state_at(seat).board.len()
                );
                let unit = BoardUnit {
                    instance_id,
                    card_id: "hero_power_token".to_string(),
                    atk,
                    hp,
                    max_hp: hp,
                    ready: false,
                    is_vehicle: false,
                    keywords: Vec::new(),
                    class: CardClass::Neutral,
                };
                self.seat_state_at_mut(seat).board.push(unit.clone());
                let mid = self.match_id.clone();
                vec![Event::OperatorSummoned(OperatorSummoned {
                    match_id: mid,
                    player: seat,
                    unit,
                })]
            }
        }
    }

    /// Grow the seat's max-Juice crystal by one, capped at `JUICE_CAP`.
    /// INDEPENDENT of how much was spent last turn — this is the fix for the
    /// pin-at-1 bug (the old `ramped_juice` grew the *remaining* pool).
    fn grown_crystal(&self, seat: Player) -> u8 {
        self.outfit_at(seat)
            .max_juice
            .saturating_add(JUICE_RAMP_PER_TURN)
            .min(JUICE_CAP)
    }

    /// Resolve `seat`'s start-of-turn draw, returning the Fatigue dealt and the
    /// drawing Boss's resulting HP. A non-empty deck yields a card and deals no
    /// Fatigue; drawing from an empty deck deals escalating Fatigue to the drawing
    /// Boss instead of yielding a card. The deck-nonempty invariant
    /// ([`GameSession::ensure_decks_nonempty`]) precludes the empty case on a
    /// legal end of turn, so it resolves to zero Fatigue with the Boss unharmed.
    fn resolve_start_of_turn_draw(&self, seat: Player) -> (i32, i32) {
        let outfit = self.outfit_at(seat);
        if outfit.deck_size == 0 {
            let fatigue = FATIGUE_PER_EMPTY_DRAW;
            (fatigue, outfit.boss_hp - fatigue)
        } else {
            (0, outfit.boss_hp)
        }
    }

    /// Handle `EndTurnCmd`: verify the command targets this match and the player
    /// whose turn it currently is; enforce every match-play invariant against the
    /// session's state; ramp the incoming seat's Juice and resolve its
    /// start-of-turn draw; and emit [`Event::FatigueDamageDealt`] followed by
    /// [`Event::TurnEnded`].
    fn end_turn(&mut self, cmd: EndTurn) -> Result<Vec<Event>, DomainError> {
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

        // Enforce every match-play invariant before passing the turn.
        self.ensure_boards_within_caps()?;
        self.ensure_heat_within_bounds()?;
        self.ensure_starting_juice_valid()?;
        self.ensure_decks_nonempty()?;
        self.ensure_heists_prereqs_satisfied()?;
        self.ensure_bosses_alive()?;
        let turn_player = self.ensure_opening_player_designated()?;

        // Turn-ownership: a turn is ended only by the player whose turn it is.
        if seat != turn_player {
            return Err(DomainError::InvariantViolation(format!(
                "player '{}' (seat {seat:?}) may not end the turn; it is player {turn_player:?}'s turn",
                cmd.player_id
            )));
        }

        // The turn passes to the opponent, whose turn now begins: their Juice
        // ramps and they resolve a start-of-turn draw.
        let incoming = Self::opponent_of(seat);
        let next_player_max_juice = self.grown_crystal(incoming);
        let next_player_juice = next_player_max_juice; // refill available TO the crystal
        let (fatigue_amount, boss_hp_remaining) = self.resolve_start_of_turn_draw(incoming);
        let incoming_player_id = self.outfit_at(incoming).name.clone();

        // Apply the passed turn to the aggregate: grow the incoming seat's
        // crystal, refill its available Juice to that crystal, apply any
        // start-of-turn Fatigue to its Boss, and hand it the turn.
        {
            let outfit = self.outfit_at_mut(incoming);
            outfit.max_juice = next_player_max_juice;
            outfit.available_juice = next_player_juice;
            outfit.boss_hp = boss_hp_remaining;
        }

        // Ready the incoming seat's board units (clear summoning sickness). This is
        // spec §1b step 4 — a unit summoned last turn becomes able to attack now.
        for u in self.seat_state_at_mut(incoming).board.iter_mut() {
            u.ready = true;
        }
        let readied = Event::OperatorsReadied(OperatorsReadied {
            match_id: cmd.match_id.clone(),
            player: incoming,
        });

        self.opening_player = Some(incoming);

        // Trademark seam (Task 7): if the incoming seat's Boss carries a
        // StartOfTurn trademark, resolve its effect and fold its deltas in,
        // reusing the same resolution as a hero power. Default `None` is a
        // no-op, so an Outfit that never sets `trademark_effect` sees no
        // behavior change — existing end_turn tests stay green. The trademark
        // catalog itself is Subsystem 2; this only lands the trigger point.
        let mut trademark_events = Vec::new();
        if let Some(TrademarkEffect {
            trigger: TrademarkTrigger::StartOfTurn,
            effect,
        }) = self.outfit_at(incoming).trademark_effect
        {
            trademark_events = self.resolve_hero_power(incoming, effect, "");
        }

        // A successful end of turn resolves the start-of-turn draw first, then
        // marks the turn passed.
        let fatigue = Event::FatigueDamageDealt(FatigueDamageDealt {
            match_id: cmd.match_id.clone(),
            player_id: incoming_player_id,
            player: incoming,
            amount: fatigue_amount,
            boss_hp_remaining,
        });
        let ended = Event::TurnEnded(TurnEnded {
            match_id: cmd.match_id,
            player_id: cmd.player_id,
            player: seat,
            next_player: incoming,
            next_player_juice,
            next_player_max_juice,
        });
        self.root.record(Box::new(readied.clone()));
        for e in &trademark_events {
            self.root.record(Box::new(e.clone()));
        }
        self.root.record(Box::new(fatigue.clone()));
        self.root.record(Box::new(ended.clone()));
        let mut all = vec![readied];
        all.append(&mut trademark_events);
        all.push(fatigue);
        all.push(ended);
        Ok(all)
    }

    /// Cop-Event-draw invariant: the Cop Event is drawn from a seeded d10 table,
    /// so the draw must be a valid d10 face, `1..=`[`COP_EVENT_DIE_SIDES`].
    fn ensure_cop_draw_valid(&self, rng_draw: u8) -> Result<(), DomainError> {
        if !(1..=COP_EVENT_DIE_SIDES).contains(&rng_draw) {
            return Err(DomainError::InvariantViolation(format!(
                "rngDraw {rng_draw} is not a valid d10 Cop Event draw; it must be within 1..={COP_EVENT_DIE_SIDES}"
            )));
        }
        Ok(())
    }

    /// Handle `ResolveCopEventCmd`: verify the command targets this match and the
    /// player whose turn it currently is, validate the seeded d10 draw, enforce
    /// every match-play invariant against the session's state, reset the seat's
    /// Heat per the rules-contract, and emit [`Event::CopEventTriggered`].
    fn resolve_cop_event(&mut self, cmd: ResolveCopEvent) -> Result<Vec<Event>, DomainError> {
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

        // The Cop Event is drawn from a seeded d10 table; the draw must be valid.
        self.ensure_cop_draw_valid(cmd.rng_draw)?;

        // Enforce every match-play invariant before resolving the Cop Event.
        self.ensure_boards_within_caps()?;
        self.ensure_heat_within_bounds()?;
        self.ensure_starting_juice_valid()?;
        self.ensure_decks_nonempty()?;
        self.ensure_heists_prereqs_satisfied()?;
        self.ensure_bosses_alive()?;
        let turn_player = self.ensure_opening_player_designated()?;

        // Turn-ownership: a Cop Event is resolved only by the player whose turn it is.
        if seat != turn_player {
            return Err(DomainError::InvariantViolation(format!(
                "player '{}' (seat {seat:?}) may not resolve a Cop Event; it is player {turn_player:?}'s turn",
                cmd.player_id
            )));
        }

        // Resolving the Cop Event resets the seat's Heat to the lower bound per
        // the rules-contract; the result stays within [`HEAT_BOUNDS`].
        let new_heat = *HEAT_BOUNDS.start();
        self.outfit_at_mut(seat).starting_heat = new_heat;

        let triggered = Event::CopEventTriggered(CopEventTriggered {
            match_id: cmd.match_id,
            player_id: cmd.player_id,
            player: seat,
            rng_draw: cmd.rng_draw,
            new_heat,
        });
        self.root.record(Box::new(triggered.clone()));
        Ok(vec![triggered])
    }

    /// Handle `ConcedeMatchCmd`: verify the command targets this match and a real
    /// player, and enforce every match-play invariant against the session's
    /// state. Concede is the rules-contract's sole exception to the
    /// whose-turn-it-is rule, so — unlike every other handler — it does *not*
    /// reject when the conceding seat does not hold the turn. On success the
    /// opposing seat is declared the sole winner and it emits
    /// [`Event::MatchCompleted`].
    fn concede_match(&mut self, cmd: ConcedeMatch) -> Result<Vec<Event>, DomainError> {
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

        // Enforce every match-play invariant before applying the concession.
        self.ensure_boards_within_caps()?;
        self.ensure_heat_within_bounds()?;
        self.ensure_starting_juice_valid()?;
        self.ensure_decks_nonempty()?;
        self.ensure_heists_prereqs_satisfied()?;
        self.ensure_bosses_alive()?;
        // A concede still requires a well-formed, in-progress match (a designated
        // whose-turn-it-is), but — being the exception to the turn-ownership rule
        // — it deliberately does *not* require the conceding seat to hold it.
        self.ensure_opening_player_designated()?;

        // Forfeiting hands the win to the opposing seat, yielding exactly one
        // winner as the match-end rules-contract requires.
        let winner = Self::opponent_of(seat);
        let winning_player_id = self.outfit_at(winner).name.clone();

        let completed = Event::MatchCompleted(MatchCompleted {
            match_id: cmd.match_id,
            conceding_player_id: cmd.player_id,
            conceding_player: seat,
            winning_player_id,
            winner,
        });
        self.root.record(Box::new(completed.clone()));
        Ok(vec![completed])
    }

    /// Handle `ResolveVenueEventCmd` (Task 10, City-pillar hook): verify the
    /// command targets this match, enforce every match-play invariant, and
    /// draw from the seeded venue event table, emitting
    /// [`Event::VenueEventResolved`]. Unlike [`GameSession::resolve_cop_event`],
    /// a venue event is a neutral, match-level draw — it names no acting
    /// player and is not gated on whose turn it is. Subsystem 1's table is a
    /// single no-op entry (the draw selects it and changes nothing);
    /// Subsystem 3 grows the table with real venue events.
    fn resolve_venue_event(&mut self, cmd: ResolveVenueEvent) -> Result<Vec<Event>, DomainError> {
        // The command must name the match this session actually records.
        if cmd.match_id != self.match_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets match '{}' but this session records '{}'",
                cmd.match_id, self.match_id
            )));
        }
        // A venue event table reference must be named.
        if cmd.event_table_ref.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "an eventTableRef must be provided".to_string(),
            ));
        }

        // Enforce every match-play invariant before resolving the venue event.
        self.ensure_boards_within_caps()?;
        self.ensure_heat_within_bounds()?;
        self.ensure_starting_juice_valid()?;
        self.ensure_decks_nonempty()?;
        self.ensure_heists_prereqs_satisfied()?;
        self.ensure_bosses_alive()?;
        self.ensure_opening_player_designated()?;

        let resolved = Event::VenueEventResolved(VenueEventResolved {
            match_id: cmd.match_id,
            event_table_ref: cmd.event_table_ref,
            rng_draw: cmd.rng_draw,
        });
        self.root.record(Box::new(resolved.clone()));
        Ok(vec![resolved])
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
            ATTACK => {
                let cmd: Attack = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed AttackCmd payload: {e}"))
                })?;
                self.declare_attack(cmd)
            }
            ACTIVATE_HERO_POWER => {
                let cmd: ActivateHeroPower =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ActivateHeroPowerCmd payload: {e}"
                        ))
                    })?;
                self.activate_hero_power(cmd)
            }
            END_TURN => {
                let cmd: EndTurn = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed EndTurnCmd payload: {e}"))
                })?;
                self.end_turn(cmd)
            }
            RESOLVE_COP_EVENT => {
                let cmd: ResolveCopEvent =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ResolveCopEventCmd payload: {e}"
                        ))
                    })?;
                self.resolve_cop_event(cmd)
            }
            CONCEDE_MATCH => {
                let cmd: ConcedeMatch = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!(
                        "malformed ConcedeMatchCmd payload: {e}"
                    ))
                })?;
                self.concede_match(cmd)
            }
            RESOLVE_VENUE_EVENT => {
                let cmd: ResolveVenueEvent =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ResolveVenueEventCmd payload: {e}"
                        ))
                    })?;
                self.resolve_venue_event(cmd)
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
    use shared::{Aggregate, Command, DomainEvent};
    use wasm_bindgen::prelude::*;

    /// A stateful `GameSession` handle the browser client drives across commands.
    ///
    /// Unlike the old payload-less `execute_command` (which spun up a *fresh empty*
    /// session and ran a bare-named command that could never exercise real rules),
    /// this threads a JSON payload through [`GameSession::execute`] against retained
    /// state — a real client-side prediction engine that reaches the identical
    /// verdict the authoritative server's `apply_action` reaches for the same input.
    #[wasm_bindgen]
    pub struct WasmGameSession(GameSession);

    #[wasm_bindgen]
    impl WasmGameSession {
        /// Open a fresh session for `match_id`, mirroring the server's match.
        #[wasm_bindgen(constructor)]
        pub fn new(match_id: String) -> WasmGameSession {
            WasmGameSession(GameSession::new(match_id))
        }

        /// Run a command by name with a JSON payload; returns the emitted
        /// event-type sequence as JSON on success (the prediction), or the
        /// domain-error text on rejection — the same decision the server's
        /// `apply_action` makes for the same input.
        pub fn execute(
            &mut self,
            command_name: String,
            payload_json: String,
        ) -> Result<JsValue, JsValue> {
            let payload = payload_json.into_bytes();
            match self.0.execute(Command::with_payload(command_name, payload)) {
                Ok(events) => {
                    let types: Vec<&'static str> = events.iter().map(|e| e.event_type()).collect();
                    serde_wasm_bindgen::to_value(&types)
                        .map_err(|e| JsValue::from_str(&e.to_string()))
                }
                Err(err) => Err(JsValue::from_str(&err.to_string())),
            }
        }
    }
}

/// WASM parity gate: a `WasmGameSession` run and a native `GameSession` run of
/// the *same* command must emit the *same* event-type sequence, proving the
/// browser prediction engine and the authoritative server agree at the event
/// level. `target_arch = "wasm32"`-gated so it compiles out of the native suite;
/// run it with `wasm-pack test --node crates/game-session -- --features wasm`.
#[cfg(all(test, target_arch = "wasm32", feature = "wasm"))]
mod wasm_tests {
    use super::wasm_bindings::WasmGameSession;
    use super::{GameSession, StartMatch};
    use shared::{Aggregate, DomainEvent};
    use wasm_bindgen_test::*;

    #[wasm_bindgen_test]
    fn wasm_start_and_play_matches_native() {
        let start = StartMatch::new("m-1", "m-1-a", "m-1-b", 0xC0FFEE);

        // Native run of a representative command.
        let mut native = GameSession::new("m-1");
        let native_events = native.execute(start.into_command()).unwrap();
        let native_types: Vec<&'static str> =
            native_events.iter().map(|e| e.event_type()).collect();

        // WASM run of the SAME command via the browser binding.
        let start_json = serde_json::to_string(&start).unwrap();
        let mut wasm = WasmGameSession::new("m-1".into());
        let wasm_result = wasm.execute("StartMatchCmd".into(), start_json).unwrap();
        let wasm_types: Vec<String> = serde_wasm_bindgen::from_value(wasm_result).unwrap();

        // Prediction == authority at the event-sequence level.
        assert_eq!(native_types, wasm_types);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_effect_maps_every_registered_effect() {
        // Coverage guard: every catalog-registered effect must map to a CardEffect,
        // so adding a REGISTERED_EFFECTS entry without a mapping fails loudly.
        for name in domain::card_definition::REGISTERED_EFFECTS {
            assert!(
                CardEffect::from_script_ref(name).is_some(),
                "registered effect {name} has no CardEffect mapping"
            );
        }
    }

    #[test]
    fn card_effect_maps_known_names() {
        assert_eq!(
            CardEffect::from_script_ref("effect.noop"),
            Some(CardEffect::None)
        );
        assert_eq!(
            CardEffect::from_script_ref("effect.deal_damage"),
            Some(CardEffect::DealDamage { amount: 0 })
        );
        assert_eq!(
            CardEffect::from_script_ref("effect.recruit_operator"),
            Some(CardEffect::Summon)
        );
        assert_eq!(
            CardEffect::from_script_ref("effect.cool"),
            Some(CardEffect::Cool { amount: 0 })
        );
        assert_eq!(CardEffect::from_script_ref("effect.unknown"), None);
    }

    #[test]
    fn keyword_parse_accepts_known_rejects_unknown() {
        assert_eq!(Keyword::parse("Spotlight").unwrap(), Keyword::Spotlight);
        assert_eq!(Keyword::parse("Drive-By").unwrap(), Keyword::DriveBy);
        assert!(Keyword::parse("Bogus").is_err());
    }

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

    // Scenario: the Global Constraint (Task 10) requires a match be startable
    // AT a venue via the command path, not only via the server-side
    // `set_location` config method. A `StartMatch` carrying `Some(location)`
    // must thread it into session state.
    #[test]
    fn start_match_command_carries_location_into_session() {
        let mut session = valid_session();
        let mut cmd = valid_cmd();
        let location = LocationModifier {
            location_id: "farm-1".into(),
            location_type: "server_farm".into(),
            class_boosts: vec![(CardClass::Hacker, 1)],
            heat_multiplier: 2,
            event_table_ref: Some("table-farm".into()),
        };
        cmd.location = Some(location.clone());

        session
            .execute(cmd.into_command())
            .expect("a valid start carrying a location should succeed");

        assert_eq!(session.location, Some(location));
    }

    // Scenario: an existing StartMatchCmd payload with no `location` field
    // still deserializes cleanly (via `#[serde(default)]`) and leaves the
    // session's location at `None`, exactly as before this field existed.
    #[test]
    fn start_match_without_location_field_deserializes_to_none() {
        let payload = serde_json::json!({
            "matchId": "m-1",
            "playerAOutfit": "m-1-a",
            "playerBOutfit": "m-1-b",
            "rngSeed": 0xC0FFEEu64,
        });
        let decoded: StartMatch = serde_json::from_value(payload).unwrap();
        assert_eq!(decoded.location, None);

        let mut session = valid_session();
        session
            .execute(decoded.into_command())
            .expect("a legacy payload with no location still starts the match");
        assert_eq!(session.location, None);
    }

    // Scenario: StartMatch deals a deterministic opening hand from a seeded deck.
    #[test]
    fn start_match_deals_opening_hands_from_seeded_deck() {
        let mut session = valid_session();
        let events = session
            .execute(valid_cmd().into_command())
            .expect("a valid StartMatch deals hands");
        assert!(events.iter().any(|e| matches!(e, Event::MatchStarted(_))));
        // Both seats hold OPENING_HAND cards; the rest is the ordered deck.
        assert_eq!(session.seat_state_at(Player::A).hand.len(), OPENING_HAND);
        assert_eq!(session.seat_state_at(Player::B).hand.len(), OPENING_HAND);
        // The remainder of the 30-card deck stays behind the hand.
        assert_eq!(
            session.seat_state_at(Player::A).deck.len(),
            30 - OPENING_HAND
        );
        // Deterministic: same seed => identical opening hand instance ids.
        let mut again = valid_session();
        again.execute(valid_cmd().into_command()).unwrap();
        let ids_a: Vec<_> = session
            .seat_state_at(Player::A)
            .hand
            .iter()
            .map(|c| c.instance_id.clone())
            .collect();
        let ids_a2: Vec<_> = again
            .seat_state_at(Player::A)
            .hand
            .iter()
            .map(|c| c.instance_id.clone())
            .collect();
        assert_eq!(ids_a, ids_a2, "the seeded deal is deterministic");
    }

    // Scenario: StartMatch's server-authoritative anti-cheat backstop rejects a
    // dealt card locked to a Boss other than the seat's own (Task 8). The
    // ported CARD_POOL never carries a boss lock, so a legitimate deal can
    // never trip this check through the public StartMatchCmd path alone;
    // this calls `ensure_boss_locks_honored` directly — the exact,
    // now-parameterized validation `start_match` runs against its LOCAL
    // deal (deck/hand built but not yet written into `self.seat_a`/
    // `self.seat_b`) before it commits anything to seat state.
    #[test]
    fn start_match_rejects_mismatched_boss_lock() {
        let session = valid_session();
        // OutfitConfig::new gives player A's Boss the name "m-1-a-boss".
        assert_eq!(session.outfit_at(Player::A).boss_name, "m-1-a-boss");
        let mismatched = CardInstance {
            instance_id: "A-locked-0".to_string(),
            card_id: "boss-card".to_string(),
            cost: 0,
            card_type: CardType::Job,
            effect: CardEffect::None,
            atk: 0,
            hp: 0,
            keywords: vec![],
            boss_lock: Some("some-other-boss".to_string()),
            class: CardClass::Neutral,
        };

        let err = GameSession::ensure_boss_locks_honored(
            Player::A,
            "m-1-a-boss",
            std::iter::once(&mismatched),
        )
        .expect_err("a card locked to a mismatched Boss must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: a legitimately dealt match (no boss-locked cards) passes the
    // boss-lock backstop cleanly, both through the public execute() path and
    // when the same check is re-run directly against the resulting seat
    // state.
    #[test]
    fn start_match_accepts_deal_with_no_boss_locked_cards() {
        let mut session = valid_session();
        session
            .execute(valid_cmd().into_command())
            .expect("a legitimate deal carries no boss-locked cards");
        for seat in [Player::A, Player::B] {
            let boss = session.outfit_at(seat).boss_name.clone();
            let st = session.seat_state_at(seat);
            assert!(GameSession::ensure_boss_locks_honored(
                seat,
                &boss,
                st.deck.iter().chain(st.hand.iter())
            )
            .is_ok());
        }
    }

    // The Rust mulberry32 port must reproduce the client's JS PRNG bit-for-bit,
    // or a WASM-predicted deal (Task 9) diverges from the server. The expected
    // f64s below were computed from web/src/match/rules.ts's mulberry32 for
    // seed 0xc0ffee (first three draws).
    #[test]
    fn mulberry32_matches_client_js_bit_for_bit() {
        let mut rng = mulberry32(0xc0ffee);
        assert_eq!(rng(), 0.021141508361324668);
        assert_eq!(rng(), 0.6661099966149777);
        assert_eq!(rng(), 0.7799714196007699);
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

    /// A session `m-1` opened cleanly with player `A` to move, both seats dealt
    /// their seeded opening hands. Effect tests push a *known* card into `A`'s
    /// hand and set `A`'s Juice before playing it.
    fn seated_match() -> GameSession {
        let mut session = GameSession::new("m-1");
        session.set_opening_player(Some(Player::A));
        session
            .execute(StartMatch::new("m-1", "m-1-a", "m-1-b", 0xC0FFEE).into_command())
            .expect("a default opening starts cleanly");
        session
    }

    /// Set `seat`'s Juice crystal and available pool to `n` (so it can afford a
    /// card costing up to `n`).
    fn give_juice(session: &mut GameSession, seat: Player, n: u8) {
        let outfit = session.outfit_at_mut(seat);
        outfit.max_juice = n;
        outfit.available_juice = n;
    }

    /// Build a hand card instance with explicit play-stats for effect tests.
    fn test_card_instance(
        id: &str,
        cost: u8,
        card_type: CardType,
        effect: CardEffect,
        atk: u8,
        hp: u8,
        kws: &[Keyword],
    ) -> CardInstance {
        CardInstance {
            instance_id: id.to_string(),
            card_id: "test".to_string(),
            cost,
            card_type,
            effect,
            atk,
            hp,
            keywords: kws.to_vec(),
            boss_lock: None,
            class: CardClass::Neutral,
        }
    }

    #[test]
    fn play_summon_card_puts_unit_on_board_unready() {
        let mut session = seated_match();
        // Put a known summon card in A's hand: a 3/2 Operator, cost 2, Summon.
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "A-homie-0",
                2,
                CardType::Operator,
                CardEffect::Summon,
                3,
                2,
                &[],
            ));
        give_juice(&mut session, Player::A, 5);

        let events = session
            .execute(PlayCard::new("m-1", "m-1-a", "A-homie-0", "boss:B", 2).into_command())
            .expect("summon is affordable");

        assert!(events
            .iter()
            .any(|e| matches!(e, Event::OperatorSummoned(s) if s.unit.instance_id == "A-homie-0" && !s.unit.ready)));
        assert!(session
            .seat_state_at(Player::A)
            .board
            .iter()
            .any(|u| u.instance_id == "A-homie-0"));
        assert!(
            session
                .seat_state_at(Player::A)
                .hand
                .iter()
                .all(|c| c.instance_id != "A-homie-0"),
            "card leaves hand"
        );
    }

    #[test]
    fn play_damage_card_hits_the_boss() {
        let mut session = seated_match();
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "A-bolt-0",
                1,
                CardType::Job,
                CardEffect::DealDamage { amount: 3 },
                0,
                0,
                &[],
            ));
        give_juice(&mut session, Player::A, 5);
        let mut b = OutfitConfig::new("m-1-b");
        b.boss_hp = 10;
        session.configure_player_b(b);

        session
            .execute(PlayCard::new("m-1", "m-1-a", "A-bolt-0", "boss:B", 1).into_command())
            .unwrap();
        assert_eq!(session.outfit_at(Player::B).boss_hp, 7, "10 - 3");
    }

    #[test]
    fn play_driveby_summon_also_hits_enemy_boss() {
        let mut session = seated_match();
        // Stolen Whip: 4/3 Vehicle, Drive-By amount 2.
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "A-whip-0",
                3,
                CardType::Vehicle,
                CardEffect::Summon,
                4,
                3,
                &[Keyword::DriveBy],
            ));
        give_juice(&mut session, Player::A, 5);
        let mut b = OutfitConfig::new("m-1-b");
        b.boss_hp = 10;
        session.configure_player_b(b);

        session
            .execute(PlayCard::new("m-1", "m-1-a", "A-whip-0", "boss:B", 3).into_command())
            .unwrap();
        assert_eq!(
            session.outfit_at(Player::B).boss_hp,
            8,
            "Drive-By strafes 2 on arrival"
        );
    }

    #[test]
    fn summon_rejected_when_operator_board_full() {
        let mut session = seated_match();
        for i in 0..MAX_OPERATORS {
            session.seat_state_at_mut(Player::A).board.push(test_unit(
                &format!("A-op-{i}"),
                1,
                1,
                true,
                false,
                &[],
            ));
        }
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "A-homie-0",
                2,
                CardType::Operator,
                CardEffect::Summon,
                3,
                2,
                &[],
            ));
        give_juice(&mut session, Player::A, 5);
        let err = session
            .execute(PlayCard::new("m-1", "m-1-a", "A-homie-0", "boss:B", 2).into_command())
            .expect_err("board full");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario: Successfully execute PlayCardCmd — a card.played event and a
    // heat.raised event are emitted.
    #[test]
    fn plays_card_and_emits_card_played_and_heat_raised_events() {
        let mut session = valid_session();
        // Seat the played instance in A's hand with a no-op effect, so only the
        // card.played + heat.raised deltas are emitted (no effect deltas).
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "card-instance-1",
                2,
                CardType::Job,
                CardEffect::None,
                0,
                0,
                &[],
            ));

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

    // play_card must DEDUCT Juice from state (it previously only emitted the spend).
    #[test]
    fn play_card_deducts_juice() {
        let mut session = valid_session();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 5;
        a.available_juice = 5;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "card-instance-1",
                3,
                CardType::Job,
                CardEffect::None,
                0,
                0,
                &[],
            ));

        session
            .execute(PlayCard::new("m-1", "m-1-a", "card-instance-1", "boss:B", 3).into_command())
            .expect("a cost-3 card is affordable at 5 Juice");

        assert_eq!(session.outfit_at(Player::A).available_juice, 2, "5 - 3 = 2");
    }

    // play_card must PERSIST the Heat raise to state (previously only in the event).
    #[test]
    fn play_card_persists_heat() {
        let mut session = valid_session();
        let mut a = OutfitConfig::new("m-1-a");
        a.starting_heat = 0;
        a.max_juice = 5;
        a.available_juice = 5;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "card-instance-1",
                1,
                CardType::Job,
                CardEffect::None,
                0,
                0,
                &[],
            ));

        session
            .execute(PlayCard::new("m-1", "m-1-a", "card-instance-1", "boss:B", 1).into_command())
            .expect("play succeeds");

        assert_eq!(
            session.outfit_at(Player::A).starting_heat,
            1,
            "Heat 0 -> 1 persisted to state"
        );
    }

    // A REJECTED play must leave available_juice unchanged (no partial mutation).
    #[test]
    fn play_card_rejection_leaves_juice_unchanged() {
        let mut session = valid_session();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 3;
        a.available_juice = 3;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));
        // Seat the played instance so the rejection is the affordability check
        // (cost 4 > available 3), proving that rejection mutates no Juice.
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "card-instance-1",
                4,
                CardType::Job,
                CardEffect::None,
                0,
                0,
                &[],
            ));

        let _ = session
            .execute(PlayCard::new("m-1", "m-1-a", "card-instance-1", "boss:B", 4).into_command())
            .expect_err("cost 4 > available 3 is rejected");

        assert_eq!(
            session.outfit_at(Player::A).available_juice,
            3,
            "rejected play must not deduct"
        );
    }

    // Scenario: rejected — a card may only be played when its Juice cost does not
    // exceed currently available Juice.
    #[test]
    fn play_card_rejects_when_cost_exceeds_available_juice() {
        let mut session = valid_session();
        // Default available Juice is 3; a cost of 4 cannot be afforded.
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "card-instance-1",
                4,
                CardType::Job,
                CardEffect::None,
                0,
                0,
                &[],
            ));
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

    // ---- AttackCmd (S-5) ---------------------------------------------------

    /// Build a board unit for combat tests.
    fn test_unit(
        id: &str,
        atk: u8,
        hp: u8,
        ready: bool,
        is_vehicle: bool,
        kws: &[Keyword],
    ) -> BoardUnit {
        BoardUnit {
            instance_id: id.to_string(),
            card_id: "test".to_string(),
            atk,
            hp,
            max_hp: hp,
            ready,
            is_vehicle,
            keywords: kws.to_vec(),
            class: CardClass::Neutral,
        }
    }

    /// Build a Hacker-class board unit for location-modifier seam tests
    /// (Task 10): ready, non-vehicle, no keywords, tagged `CardClass::Hacker`.
    fn hacker_unit(id: &str, atk: u8, hp: u8) -> BoardUnit {
        let mut unit = test_unit(id, atk, hp, true, false, &[]);
        unit.class = CardClass::Hacker;
        unit
    }

    #[test]
    fn attack_unit_is_simultaneous_with_retaliation() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        // A attacker 3/2, B defender 2/5.
        session
            .seat_state_at_mut(Player::A)
            .board
            .push(test_unit("A-atk", 3, 2, true, false, &[]));
        session
            .seat_state_at_mut(Player::B)
            .board
            .push(test_unit("B-def", 2, 5, true, false, &[]));

        let events = session
            .execute(Attack::new("m-1", "m-1-a", "A-atk", "op:B-def").into_command())
            .expect("A attacks B's unit");

        // Defender took 3 (5 -> 2); attacker took retaliation 2 (2 -> 0) and died.
        assert!(events.iter().any(
            |e| matches!(e, Event::OperatorDamaged(d) if d.instance_id == "B-def" && d.new_hp == 2)
        ));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::OperatorDied(d) if d.instance_id == "A-atk")));
        assert!(session
            .seat_state_at(Player::A)
            .board
            .iter()
            .all(|u| u.instance_id != "A-atk"));
    }

    #[test]
    fn attack_boss_reduces_hp_and_ends_match_at_zero() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        let mut b = OutfitConfig::new("m-1-b");
        b.boss_hp = 3;
        session.configure_player_b(b);
        session
            .seat_state_at_mut(Player::A)
            .board
            .push(test_unit("A-atk", 5, 5, true, false, &[]));

        let events = session
            .execute(Attack::new("m-1", "m-1-a", "A-atk", "boss:B").into_command())
            .expect("A attacks B's boss");

        assert!(events
            .iter()
            .any(|e| matches!(e, Event::BossDamaged(d) if d.new_hp == 0)));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::BossDefeated(d) if d.winner == Player::A)));
    }

    #[test]
    fn spotlight_forces_attack_onto_taunt_unit() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        session
            .seat_state_at_mut(Player::A)
            .board
            .push(test_unit("A-atk", 2, 2, true, false, &[]));
        session.seat_state_at_mut(Player::B).board.push(test_unit(
            "B-taunt",
            0,
            4,
            true,
            false,
            &[Keyword::Spotlight],
        ));
        // Attacking the boss while a Spotlight unit stands is rejected.
        let err = session
            .execute(Attack::new("m-1", "m-1-a", "A-atk", "boss:B").into_command())
            .expect_err("must hit the Spotlight first");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn attack_cmd_is_recognized_not_unknown() {
        let mut session = valid_session();
        let err = session.execute(Attack::new("m-1", "m-1-a", "x", "boss:B").into_command());
        // Whatever the rejection reason, it must NOT be UnknownCommand — the rename worked.
        assert!(!matches!(err, Err(DomainError::UnknownCommand { .. })));
    }

    /// A legal `AttackCmd` for `m-1`: the turn-holding player `A`
    /// commits an attacker against player `B`'s Boss target.
    fn valid_attack() -> Attack {
        Attack::new("m-1", "m-1-a", "attacker-1", "boss:B")
    }

    // Scenario: attacking a Boss below lethal reduces its HP, does not defeat
    // it, and exhausts the surviving attacker. (Rewritten from the removed
    // boss-instakill happy path, which set boss_hp = 0 and emitted
    // combat.resolved + boss.defeated unconditionally.)
    #[test]
    fn attack_boss_deals_nonlethal_damage_and_exhausts_attacker() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        // Default boss_hp is 30; a 4-atk attacker leaves it alive at 26.
        session
            .seat_state_at_mut(Player::A)
            .board
            .push(test_unit("A-atk", 4, 5, true, false, &[]));

        let events = session
            .execute(Attack::new("m-1", "m-1-a", "A-atk", "boss:B").into_command())
            .expect("a valid attack should succeed");

        assert!(events.iter().any(
            |e| matches!(e, Event::BossDamaged(d) if d.player == Player::B && d.new_hp == 26)
        ));
        assert!(!events.iter().any(|e| matches!(e, Event::BossDefeated(_))));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::OperatorExhausted(d) if d.instance_id == "A-atk")));
        // The attacker survived and is now exhausted (not ready).
        let attacker = session
            .seat_state_at(Player::A)
            .board
            .iter()
            .find(|u| u.instance_id == "A-atk")
            .expect("attacker survived the boss trade");
        assert!(!attacker.ready);
        assert_eq!(session.uncommitted_events().len(), events.len());
    }

    // Scenario: rejected — Juice starts at 1 and remains hard-capped at 10.
    #[test]
    fn declare_attack_rejects_when_starting_juice_is_not_one() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_juice = 3;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_attack().into_command())
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
            .execute(valid_attack().into_command())
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
            .execute(valid_attack().into_command())
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
            .execute(valid_attack().into_command())
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
            .execute(valid_attack().into_command())
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
            .execute(valid_attack().into_command())
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
            .execute(valid_attack().into_command())
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
            .execute(Attack::new("m-1", "m-1-b", "attacker-1", "boss:A").into_command())
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
            .execute(valid_attack().into_command())
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
            .execute(valid_attack().into_command())
            .expect_err("a defeated Boss must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An attack must name the match this session records.
    #[test]
    fn declare_attack_rejects_when_command_targets_a_different_match() {
        let mut session = valid_session();
        let err = session
            .execute(Attack::new("other-match", "m-1-a", "attacker-1", "boss:B").into_command())
            .expect_err("a mismatched match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An attack must name a configured Outfit.
    #[test]
    fn declare_attack_rejects_unknown_player() {
        let mut session = valid_session();
        let err = session
            .execute(Attack::new("m-1", "ghost", "attacker-1", "boss:B").into_command())
            .expect_err("an unknown player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An attack must identify the attacking combatant.
    #[test]
    fn declare_attack_rejects_blank_attacker_id() {
        let mut session = valid_session();
        let err = session
            .execute(Attack::new("m-1", "m-1-a", " ", "boss:B").into_command())
            .expect_err("a blank attackerId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a malformed target_ref (neither "boss:" nor "op:")
    // is invalid. (Rewritten from the removed blank-defender_id test: real
    // combat parses a target_ref rather than validating a defeated-Boss name.)
    #[test]
    fn attack_rejects_malformed_target_ref() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        session
            .seat_state_at_mut(Player::A)
            .board
            .push(test_unit("A-atk", 2, 2, true, false, &[]));

        let err = session
            .execute(Attack::new("m-1", "m-1-a", "A-atk", "").into_command())
            .expect_err("a malformed targetRef must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — an "op:" target that names no enemy unit is invalid.
    // (Rewritten from the removed non-opposing-Boss defender_id test.)
    #[test]
    fn attack_rejects_nonexistent_op_target() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        session
            .seat_state_at_mut(Player::A)
            .board
            .push(test_unit("A-atk", 2, 2, true, false, &[]));

        let err = session
            .execute(Attack::new("m-1", "m-1-a", "A-atk", "op:ghost").into_command())
            .expect_err("an op target naming no unit must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn attack_command_payload_round_trips() {
        let cmd = valid_attack();
        let command = cmd.into_command();
        assert_eq!(command.name, Attack::COMMAND);
        let decoded: Attack = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_attack());
    }

    // ---- ActivateHeroPowerCmd (S-6) ----------------------------------------

    /// A legal `ActivateHeroPowerCmd` for `m-1`: the turn-holding player `A`
    /// activates their Boss hero power at a target, paying 2 Juice (within the
    /// default available pool of 3). Tests mutate one aspect at a time to drive
    /// a rejection.
    fn valid_activate_hero_power() -> ActivateHeroPower {
        ActivateHeroPower::new("m-1", "m-1-a", "target-1", 2)
    }

    // Scenario: Successfully execute ActivateHeroPowerCmd — a
    // hero_power.activated event is emitted, the GameSession state is updated,
    // and (Task 7) the default DealDamage{2} hero-power effect resolves against
    // the target, following with a boss.damaged delta.
    #[test]
    fn activates_hero_power_and_emits_hero_power_activated_event() {
        let mut session = valid_session();

        let events = session
            .execute(valid_activate_hero_power().into_command())
            .expect("a valid hero power activation should succeed");

        // HeroPowerActivated first, then the resolved effect's delta.
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type(), "hero_power.activated");
        match &events[0] {
            Event::HeroPowerActivated(activated) => {
                assert_eq!(activated.match_id, "m-1");
                assert_eq!(activated.player_id, "m-1-a");
                assert_eq!(activated.player, Player::A);
                assert_eq!(activated.target_ref, "target-1");
                assert_eq!(activated.juice_spent, 2);
                // Default available Juice is 3; paying 2 leaves 1.
                assert_eq!(activated.remaining_juice, 1);
            }
            other => panic!("expected HeroPowerActivated, got {other:?}"),
        }
        // The default hero-power effect (DealDamage{2}) resolves against the
        // enemy Boss (targetRef "target-1" is not an "op:" ref), dealing 2.
        assert_eq!(events[1].event_type(), "boss.damaged");
        match &events[1] {
            Event::BossDamaged(damaged) => {
                assert_eq!(damaged.player, Player::B);
                assert_eq!(damaged.amount, 2);
                assert_eq!(damaged.new_hp, 28); // default boss_hp 30 - 2
            }
            other => panic!("expected BossDamaged, got {other:?}"),
        }
        // The paid Juice cost is deducted from the seat's available pool — the
        // GameSession state is updated — and both events advance the version.
        assert_eq!(session.version(), 2);
        assert_eq!(session.uncommitted_events().len(), 2);
        assert_eq!(
            session.uncommitted_events()[0].event_type(),
            "hero_power.activated"
        );
        assert_eq!(session.uncommitted_events()[1].event_type(), "boss.damaged");
    }

    // Scenario: rejected — a hero power may only be activated when its Juice cost
    // does not exceed currently available Juice.
    #[test]
    fn activate_hero_power_rejects_when_cost_exceeds_available_juice() {
        let mut session = valid_session();
        // Default available Juice is 3; a cost of 4 cannot be afforded.
        let err = session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "target-1", 4).into_command())
            .expect_err("a hero power the player cannot afford must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a board may hold at most 7 Operators and 3 Vehicles.
    #[test]
    fn activate_hero_power_rejects_when_board_exceeds_operator_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.operators = MAX_OPERATORS + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_activate_hero_power().into_command())
            .expect_err("an over-capacity board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn activate_hero_power_rejects_when_board_exceeds_vehicle_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.vehicles = MAX_VEHICLES + 1;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_activate_hero_power().into_command())
            .expect_err("an over-capacity vehicle board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Heat is bounded 0..10 and no state may leave it.
    #[test]
    fn activate_hero_power_rejects_when_heat_leaves_bounds() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_heat = *HEAT_BOUNDS.end() + 1; // Outside [0, 10].
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_activate_hero_power().into_command())
            .expect_err("Heat outside its bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a Heist resolves only after its prerequisite queue is
    // satisfied.
    #[test]
    fn activate_hero_power_rejects_when_heist_resolved_with_outstanding_prereqs() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.heist_resolved = true;
        outfit.outstanding_heist_prereqs = 2;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_activate_hero_power().into_command())
            .expect_err("a Heist resolved with outstanding prereqs must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — drawing from an empty deck deals Fatigue instead of a
    // card, so a match may not carry a deckless Outfit.
    #[test]
    fn activate_hero_power_rejects_when_deck_is_empty() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.deck_size = 0;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_activate_hero_power().into_command())
            .expect_err("an empty deck must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a command is valid only for the player whose turn it
    // currently is; an activation by the off-turn player is rejected.
    #[test]
    fn activate_hero_power_rejects_when_not_the_players_turn() {
        let mut session = valid_session();
        // Player A holds the turn; player B tries to activate a hero power.
        let err = session
            .execute(ActivateHeroPower::new("m-1", "m-1-b", "target-1", 2).into_command())
            .expect_err("an off-turn activation must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A turn-less setup is likewise ill-formed for a hero power activation.
    #[test]
    fn activate_hero_power_rejects_when_no_opening_player_is_designated() {
        let mut session = valid_session();
        session.set_opening_player(None);

        let err = session
            .execute(valid_activate_hero_power().into_command())
            .expect_err("a turn-less setup must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a match ends the instant a Boss's HP reaches 0 or
    // below, so a defeated Boss cannot be carried into an activation.
    #[test]
    fn activate_hero_power_rejects_when_a_boss_is_defeated() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.boss_hp = 0;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_activate_hero_power().into_command())
            .expect_err("a defeated Boss must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Juice starts at 1; an opening Juice that is not the
    // starting value is an illegal Juice state.
    #[test]
    fn activate_hero_power_rejects_when_starting_juice_is_not_one() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_juice = 3;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_activate_hero_power().into_command())
            .expect_err("an illegal opening Juice must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An activation must name the match this session records.
    #[test]
    fn activate_hero_power_rejects_when_command_targets_a_different_match() {
        let mut session = valid_session();
        let err = session
            .execute(ActivateHeroPower::new("other-match", "m-1-a", "target-1", 2).into_command())
            .expect_err("a mismatched match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An activation must name a configured Outfit.
    #[test]
    fn activate_hero_power_rejects_unknown_player() {
        let mut session = valid_session();
        let err = session
            .execute(ActivateHeroPower::new("m-1", "ghost", "target-1", 2).into_command())
            .expect_err("an unknown player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An activation must identify the target it resolves against.
    #[test]
    fn activate_hero_power_rejects_blank_target_ref() {
        let mut session = valid_session();
        let err = session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "  ", 2).into_command())
            .expect_err("a blank targetRef must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn activate_hero_power_command_payload_round_trips() {
        let cmd = valid_activate_hero_power();
        let command = cmd.into_command();
        assert_eq!(command.name, ActivateHeroPower::COMMAND);
        let decoded: ActivateHeroPower = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_activate_hero_power());
    }

    // Scenario (Task 7): activating the hero power resolves its declared
    // HeroPowerEffect (DealDamage) against the target, on top of the existing
    // Juice deduction/HeroPowerActivated event.
    #[test]
    fn hero_power_deals_declared_damage_and_spends_juice() {
        let mut session = seated_match();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 5;
        a.available_juice = 5;
        a.hero_power_effect = HeroPowerEffect::DealDamage { amount: 2 };
        a.hero_power_cost = 2;
        session.configure_player_a(a);
        let mut b = OutfitConfig::new("m-1-b");
        b.boss_hp = 10;
        session.configure_player_b(b);
        session.set_opening_player(Some(Player::A));

        let events = session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "boss:B", 2).into_command())
            .expect("affordable hero power");

        assert!(events
            .iter()
            .any(|e| matches!(e, Event::BossDamaged(d) if d.amount == 2 && d.new_hp == 8)));
        assert_eq!(session.outfit_at(Player::A).available_juice, 3, "5 - 2");
    }

    // Scenario (Task 7): a command that claims a lower cost than the Boss's
    // declared hero_power_cost is rejected outright (anti-cheat) — it never
    // reaches the Juice deduction or effect resolution.
    #[test]
    fn hero_power_rejects_understated_cost() {
        let mut session = seated_match();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 5;
        a.available_juice = 5;
        a.hero_power_cost = 2;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));
        let err = session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "boss:B", 0).into_command())
            .expect_err("client cannot understate the declared cost");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // Scenario (Task 7, updated by Task 7.5 remediation): a GainArmor hero
    // power raises the activating seat's own Boss HP, no target-side event,
    // but now DOES emit its own BossArmorGained delta (Task 7.5) so online
    // clients can reconstruct the boss_hp change.
    #[test]
    fn hero_power_gain_armor_raises_own_boss_hp() {
        let mut session = seated_match();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 5;
        a.available_juice = 5;
        a.boss_hp = 20;
        a.hero_power_effect = HeroPowerEffect::GainArmor { amount: 5 };
        a.hero_power_cost = 2;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));

        let events = session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "boss:B", 2).into_command())
            .expect("affordable hero power");

        assert_eq!(
            events.len(),
            2,
            "HeroPowerActivated + BossArmorGained (Task 7.5)"
        );
        assert_eq!(session.outfit_at(Player::A).boss_hp, 25);
    }

    // Scenario (Task 7): a Cool hero power lowers the activating seat's own
    // Heat, floored at HEAT_BOUNDS's start (never negative).
    #[test]
    fn hero_power_cool_lowers_own_heat_floored_at_bounds_start() {
        let mut session = seated_match();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 5;
        a.available_juice = 5;
        a.starting_heat = 1;
        a.hero_power_effect = HeroPowerEffect::Cool { amount: 5 };
        a.hero_power_cost = 2;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));

        session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "boss:B", 2).into_command())
            .expect("affordable hero power");

        assert_eq!(
            session.outfit_at(Player::A).starting_heat,
            *HEAT_BOUNDS.start()
        );
    }

    // Scenario (Task 7): a SummonToken hero power puts an unready token unit on
    // the activating seat's board, emitting operator.summoned after
    // hero_power.activated.
    #[test]
    fn hero_power_summon_token_puts_unready_unit_on_board() {
        let mut session = seated_match();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 5;
        a.available_juice = 5;
        a.hero_power_effect = HeroPowerEffect::SummonToken { atk: 1, hp: 1 };
        a.hero_power_cost = 2;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));

        let events = session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "boss:B", 2).into_command())
            .expect("affordable hero power");

        assert!(events
            .iter()
            .any(|e| matches!(e, Event::OperatorSummoned(s) if s.unit.atk == 1 && s.unit.hp == 1 && !s.unit.ready)));
        assert_eq!(session.seat_state_at(Player::A).board.len(), 1);
    }

    // Scenario (Task 7): SummonToken does NOT reject the activation when the
    // board is already at the Operator cap — the token is simply not summoned
    // (mirrors Hearthstone hero powers), and the Juice is still spent.
    #[test]
    fn hero_power_summon_token_skips_silently_when_board_is_full() {
        let mut session = seated_match();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 5;
        a.available_juice = 5;
        a.hero_power_effect = HeroPowerEffect::SummonToken { atk: 1, hp: 1 };
        a.hero_power_cost = 2;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));
        for i in 0..MAX_OPERATORS {
            session.seat_state_at_mut(Player::A).board.push(test_unit(
                &format!("A-filler-{i}"),
                1,
                1,
                true,
                false,
                &[],
            ));
        }

        let events = session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "boss:B", 2).into_command())
            .expect("a full board does not reject the hero power activation");

        assert_eq!(
            events.len(),
            1,
            "no OperatorSummoned when the board is full"
        );
        assert_eq!(session.seat_state_at(Player::A).board.len(), MAX_OPERATORS);
        assert_eq!(
            session.outfit_at(Player::A).available_juice,
            3,
            "Juice is still spent"
        );
    }

    // Scenario (Task 7.5 remediation): a GainJuice card raises available_juice
    // but previously emitted no delta, desyncing online clients. Playing it now
    // emits a JuiceGained event carrying the resulting (capped) available_juice.
    #[test]
    fn play_gain_juice_card_emits_juice_gained_delta() {
        let mut session = seated_match();
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "A-comeup-0",
                2,
                CardType::Piece,
                CardEffect::GainJuice { amount: 2 },
                0,
                0,
                &[],
            ));
        give_juice(&mut session, Player::A, 5);

        // Deviation from the brief's verbatim "": play_card unconditionally
        // rejects an empty targetRef (a pre-existing, Task-7.5-unrelated
        // invariant) even though resolve_effect's GainJuice arm never reads
        // it; "self" is an inert placeholder that only satisfies that check.
        let events = session
            .execute(PlayCard::new("m-1", "m-1-a", "A-comeup-0", "self", 2).into_command())
            .expect("gain-juice card plays");

        // available was 5, spent 2 (cost) -> 3, then +2 gained -> 5 (capped at 10).
        assert!(events.iter().any(|e| matches!(e, Event::JuiceGained(j) if j.player == Player::A && j.amount == 2 && j.new_juice == 5)));
    }

    // Scenario (Task 7.5 remediation): a Cool card lowers starting_heat but
    // previously emitted no delta. Playing it now emits a HeatSet event
    // carrying the resulting (floored) starting_heat.
    #[test]
    fn play_cool_card_emits_heat_set_delta() {
        let mut session = seated_match();
        // Seat A starts with some heat so Cool has something to lower.
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 5;
        a.available_juice = 5;
        a.starting_heat = 4;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));
        session
            .seat_state_at_mut(Player::A)
            .hand
            .push(test_card_instance(
                "A-crib-0",
                2,
                CardType::Piece,
                CardEffect::Cool { amount: 2 },
                0,
                0,
                &[],
            ));

        // Deviation from the brief's verbatim "": see the identical note in
        // play_gain_juice_card_emits_juice_gained_delta above.
        let events = session
            .execute(PlayCard::new("m-1", "m-1-a", "A-crib-0", "self", 2).into_command())
            .expect("cool card plays");

        // Heat 4, +1 from playing the card (HEAT_PER_PLAY), then Cool -2 -> 3.
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::HeatSet(h) if h.player == Player::A && h.new_heat == 3)));
    }

    // Scenario (Task 7.5 remediation): a GainArmor hero power raises boss_hp
    // but previously emitted no delta. Activating it now emits a
    // BossArmorGained event carrying the resulting boss_hp.
    #[test]
    fn hero_power_gain_armor_emits_boss_armor_gained_delta() {
        let mut session = seated_match();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 5;
        a.available_juice = 5;
        a.boss_hp = 30;
        a.hero_power_effect = domain::boss_definition::HeroPowerEffect::GainArmor { amount: 4 };
        a.hero_power_cost = 2;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));

        let events = session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "boss:A", 2).into_command())
            .expect("gain-armor hero power");

        assert!(events.iter().any(|e| matches!(e, Event::BossArmorGained(b) if b.player == Player::A && b.amount == 4 && b.new_hp == 34)));
        assert_eq!(session.outfit_at(Player::A).boss_hp, 34);
    }

    // Scenario (Task 7): end_turn's start-of-turn trademark seam resolves the
    // incoming seat's Boss trademark when it is Some(StartOfTurn), folding its
    // deltas into the returned events.
    #[test]
    fn end_turn_resolves_incoming_seats_start_of_turn_trademark() {
        let mut session = seated_match();
        let mut b = OutfitConfig::new("m-1-b");
        b.boss_hp = 10;
        b.trademark_effect = Some(TrademarkEffect {
            trigger: TrademarkTrigger::StartOfTurn,
            effect: HeroPowerEffect::DealDamage { amount: 3 },
        });
        session.configure_player_b(b);
        session.set_opening_player(Some(Player::A));

        let events = session
            .execute(EndTurn::new("m-1", "m-1-a").into_command())
            .expect("a legal end of turn should succeed");

        // The trademark deals 3 to the enemy Boss (A) — B's own trademark hits
        // the opponent, same targeting convention as a hero power's default.
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::BossDamaged(d) if d.amount == 3 && d.player == Player::A)));
    }

    // A default (None) trademark_effect is a no-op — the existing end_turn
    // tests (elsewhere in this module) already cover this, but this test pins
    // the exact event count/shape so a future change to the seam is caught.
    #[test]
    fn end_turn_with_no_trademark_effect_emits_no_extra_events() {
        let mut session = seated_match();
        session.set_opening_player(Some(Player::A));

        let events = session
            .execute(EndTurn::new("m-1", "m-1-a").into_command())
            .expect("a legal end of turn should succeed");

        assert_eq!(
            events.len(),
            3,
            "readied, fatigue, ended — no trademark delta"
        );
    }

    // ---- EndTurnCmd (S-7) ---------------------------------------------------

    /// A legal `EndTurnCmd` for `m-1`: the turn-holding player `A` passes the
    /// turn. Tests mutate one aspect at a time to drive a rejection.
    fn valid_end_turn() -> EndTurn {
        EndTurn::new("m-1", "m-1-a")
    }

    // Scenario: Successfully execute EndTurnCmd — an operators.readied event, a
    // fatigue.damage.dealt event, and a turn.ended event are emitted.
    #[test]
    fn ends_turn_and_emits_fatigue_damage_dealt_and_turn_ended_events() {
        let mut session = valid_session();

        let events = session
            .execute(valid_end_turn().into_command())
            .expect("a valid end of turn should succeed");

        // Three events: the incoming seat is readied first, then its
        // start-of-turn draw resolves, then the turn is marked passed.
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type(), "operators.readied");
        assert_eq!(events[1].event_type(), "fatigue.damage.dealt");
        assert_eq!(events[2].event_type(), "turn.ended");
        match &events[0] {
            Event::OperatorsReadied(readied) => {
                assert_eq!(readied.match_id, "m-1");
                assert_eq!(readied.player, Player::B);
            }
            other => panic!("expected OperatorsReadied, got {other:?}"),
        }
        match &events[1] {
            Event::FatigueDamageDealt(fatigue) => {
                assert_eq!(fatigue.match_id, "m-1");
                // The turn passes to player B, who draws at the start of its turn.
                assert_eq!(fatigue.player_id, "m-1-b");
                assert_eq!(fatigue.player, Player::B);
                // A non-empty deck yields a card, so no Fatigue is dealt and the
                // drawing Boss keeps its opening HP.
                assert_eq!(fatigue.amount, 0);
                assert_eq!(fatigue.boss_hp_remaining, 30);
            }
            other => panic!("expected FatigueDamageDealt, got {other:?}"),
        }
        match &events[2] {
            Event::TurnEnded(ended) => {
                assert_eq!(ended.match_id, "m-1");
                assert_eq!(ended.player_id, "m-1-a");
                assert_eq!(ended.player, Player::A);
                assert_eq!(ended.next_player, Player::B);
                // Default available Juice is 3; ramping +1 for the turn now
                // beginning leaves 4, within the hard cap.
                assert_eq!(ended.next_player_juice, 4);
            }
            other => panic!("expected TurnEnded, got {other:?}"),
        }
        // Three events recorded on the root: the version advances by three.
        assert_eq!(session.version(), 3);
        assert_eq!(session.uncommitted_events().len(), 3);
        assert_eq!(
            session.uncommitted_events()[0].event_type(),
            "operators.readied"
        );
        assert_eq!(
            session.uncommitted_events()[1].event_type(),
            "fatigue.damage.dealt"
        );
        assert_eq!(session.uncommitted_events()[2].event_type(), "turn.ended");
    }

    // The incoming seat's Juice ramps but stays hard-capped at JUICE_CAP.
    #[test]
    fn end_turn_ramps_incoming_juice_capped_at_the_hard_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        // Crystal already at the cap; growing it cannot exceed the cap. (The
        // crystal, not the spent-down pool, is what ramps — see grown_crystal.)
        outfit.max_juice = JUICE_CAP;
        outfit.available_juice = JUICE_CAP;
        session.configure_player_b(outfit);

        let events = session
            .execute(valid_end_turn().into_command())
            .expect("ending the turn should succeed");
        // events[0] is now OperatorsReadied, events[1] FatigueDamageDealt; TurnEnded
        // moved to events[2] with the new operators.readied delta.
        match &events[2] {
            Event::TurnEnded(ended) => {
                assert_eq!(ended.next_player_max_juice, JUICE_CAP);
                assert_eq!(ended.next_player_juice, JUICE_CAP);
            }
            other => panic!("expected TurnEnded, got {other:?}"),
        }
    }

    // Scenario: rejected — Juice starts at 1 (hard-capped at 10).
    #[test]
    fn end_turn_rejects_when_starting_juice_is_not_one() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_juice = 3;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_end_turn().into_command())
            .expect_err("an illegal opening Juice must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn end_turn_rejects_when_available_juice_exceeds_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.available_juice = JUICE_CAP + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_end_turn().into_command())
            .expect_err("available Juice over the hard cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Regression: the pin-at-1 Juice bug. A seat that emptied its pool must
    // refill to its GROWN crystal next turn, not to `spent + 1`.
    #[test]
    fn end_turn_grows_incoming_crystal_and_refills_available() {
        let mut session = valid_session();
        // Seat A is opening; seat B (incoming) has a mid-game crystal of 3 but an
        // emptied pool (spent to 0 last turn).
        let mut b = OutfitConfig::new("m-1-b");
        b.max_juice = 3;
        b.available_juice = 0;
        session.configure_player_b(b);
        session.set_opening_player(Some(Player::A));

        let events = session
            .execute(EndTurn::new("m-1", "m-1-a").into_command())
            .expect("A may end its turn");

        // Find the TurnEnded event and assert the crystal grew to 4 and available
        // refilled to the crystal (4), NOT to 1.
        let ended = events
            .iter()
            .find_map(|e| match e {
                Event::TurnEnded(t) => Some(t),
                _ => None,
            })
            .expect("end_turn emits TurnEnded");
        assert_eq!(ended.next_player_max_juice, 4, "crystal grows 3 -> 4");
        assert_eq!(
            ended.next_player_juice, 4,
            "available refills to the grown crystal, not to 1"
        );
        // State was mutated on the incoming seat.
        assert_eq!(session.outfit_at(Player::B).max_juice, 4);
        assert_eq!(session.outfit_at(Player::B).available_juice, 4);
    }

    // Scenario: rejected — a board may hold at most 7 Operators and 3 Vehicles.
    #[test]
    fn end_turn_rejects_when_board_exceeds_operator_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.operators = MAX_OPERATORS + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_end_turn().into_command())
            .expect_err("an over-capacity board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn end_turn_rejects_when_board_exceeds_vehicle_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.vehicles = MAX_VEHICLES + 1;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_end_turn().into_command())
            .expect_err("an over-capacity vehicle board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Heat is bounded 0..10 and no state may leave it.
    #[test]
    fn end_turn_rejects_when_heat_leaves_bounds() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_heat = *HEAT_BOUNDS.end() + 1; // Outside [0, 10].
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_end_turn().into_command())
            .expect_err("Heat outside its bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a Heist resolves only after its prerequisite queue is
    // satisfied.
    #[test]
    fn end_turn_rejects_when_heist_resolved_with_outstanding_prereqs() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.heist_resolved = true;
        outfit.outstanding_heist_prereqs = 2;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_end_turn().into_command())
            .expect_err("a Heist resolved with outstanding prereqs must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — drawing from an empty deck deals Fatigue instead of a
    // card, so a match may not carry a deckless Outfit.
    #[test]
    fn end_turn_rejects_when_deck_is_empty() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.deck_size = 0;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_end_turn().into_command())
            .expect_err("an empty deck must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a command is valid only for the player whose turn it
    // currently is; an end of turn by the off-turn player is rejected.
    #[test]
    fn end_turn_rejects_when_not_the_players_turn() {
        let mut session = valid_session();
        // Player A holds the turn; player B tries to end the turn.
        let err = session
            .execute(EndTurn::new("m-1", "m-1-b").into_command())
            .expect_err("an off-turn end of turn must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A turn-less setup is likewise ill-formed for ending a turn.
    #[test]
    fn end_turn_rejects_when_no_opening_player_is_designated() {
        let mut session = valid_session();
        session.set_opening_player(None);

        let err = session
            .execute(valid_end_turn().into_command())
            .expect_err("a turn-less setup must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a match ends the instant a Boss's HP reaches 0 or
    // below, so a defeated Boss cannot be carried into an end of turn.
    #[test]
    fn end_turn_rejects_when_a_boss_is_defeated() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.boss_hp = 0;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_end_turn().into_command())
            .expect_err("a defeated Boss must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An end of turn must name the match this session records.
    #[test]
    fn end_turn_rejects_when_command_targets_a_different_match() {
        let mut session = valid_session();
        let err = session
            .execute(EndTurn::new("other-match", "m-1-a").into_command())
            .expect_err("a mismatched match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An end of turn must name a configured Outfit.
    #[test]
    fn end_turn_rejects_unknown_player() {
        let mut session = valid_session();
        let err = session
            .execute(EndTurn::new("m-1", "ghost").into_command())
            .expect_err("an unknown player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // An end of turn must identify the player passing the turn.
    #[test]
    fn end_turn_rejects_blank_player_id() {
        let mut session = valid_session();
        let err = session
            .execute(EndTurn::new("m-1", "  ").into_command())
            .expect_err("a blank playerId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn end_turn_command_payload_round_trips() {
        let cmd = valid_end_turn();
        let command = cmd.into_command();
        assert_eq!(command.name, EndTurn::COMMAND);
        let decoded: EndTurn = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_end_turn());
    }

    // Regression for the readying gap: at the start of a seat's turn, that seat's
    // board units are readied (summoning sickness cleared) so a unit summoned last
    // turn can attack this turn.
    #[test]
    fn end_turn_readies_incoming_seats_units() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        // B (the incoming seat) has an UNREADY unit (as if summoned last turn).
        session
            .seat_state_at_mut(Player::B)
            .board
            .push(test_unit("B-op", 2, 2, false, false, &[]));

        let events = session
            .execute(EndTurn::new("m-1", "m-1-a").into_command())
            .expect("A ends its turn; B becomes active");

        // The incoming seat's unit is now ready...
        assert!(
            session
                .seat_state_at(Player::B)
                .board
                .iter()
                .all(|u| u.ready),
            "incoming seat's units must be readied at turn start"
        );
        // ...and an OperatorsReadied delta was emitted for B.
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::OperatorsReadied(r) if r.player == Player::B)));
    }

    // The OUTGOING seat's units are NOT readied by the opponent's turn start.
    #[test]
    fn end_turn_does_not_ready_outgoing_seats_units() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        session
            .seat_state_at_mut(Player::A)
            .board
            .push(test_unit("A-op", 2, 2, false, false, &[]));

        session
            .execute(EndTurn::new("m-1", "m-1-a").into_command())
            .expect("A ends its turn");

        // A is now the outgoing seat; its freshly-summoned unit stays unready until A's next turn.
        assert!(
            session
                .seat_state_at(Player::A)
                .board
                .iter()
                .all(|u| !u.ready),
            "the outgoing seat's units must not be readied by the opponent's turn start"
        );
    }

    // ---- ResolveCopEventCmd (S-8) ------------------------------------------

    /// A legal `ResolveCopEventCmd` for `m-1`: the turn-holding player `A`
    /// resolves the Cop Event with a valid seeded d10 draw. Tests mutate one
    /// aspect at a time to drive a rejection.
    fn valid_resolve_cop_event() -> ResolveCopEvent {
        ResolveCopEvent::new("m-1", "m-1-a", 7)
    }

    // Scenario: Successfully execute ResolveCopEventCmd — a cop.event.triggered
    // event is emitted and the GameSession state is updated.
    #[test]
    fn resolves_cop_event_and_emits_cop_event_triggered_event() {
        let mut session = valid_session();

        let events = session
            .execute(valid_resolve_cop_event().into_command())
            .expect("a valid Cop Event resolution should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "cop.event.triggered");
        match &events[0] {
            Event::CopEventTriggered(triggered) => {
                assert_eq!(triggered.match_id, "m-1");
                assert_eq!(triggered.player_id, "m-1-a");
                assert_eq!(triggered.player, Player::A);
                assert_eq!(triggered.rng_draw, 7);
                // The rules-contract resets Heat to the lower bound on resolution.
                assert_eq!(triggered.new_heat, *HEAT_BOUNDS.start());
            }
            other => panic!("expected CopEventTriggered, got {other:?}"),
        }
        // The single event advances the version and is recorded on the root.
        assert_eq!(session.version(), 1);
        assert_eq!(session.uncommitted_events().len(), 1);
        assert_eq!(
            session.uncommitted_events()[0].event_type(),
            "cop.event.triggered"
        );
    }

    // A Cop Event draw is a seeded d10; the extreme faces are both valid.
    #[test]
    fn resolve_cop_event_accepts_the_d10_bounds() {
        for draw in [1, COP_EVENT_DIE_SIDES] {
            let mut session = valid_session();
            let events = session
                .execute(ResolveCopEvent::new("m-1", "m-1-a", draw).into_command())
                .expect("a valid d10 face should resolve");
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].event_type(), "cop.event.triggered");
        }
    }

    // A draw of 0 is not a valid d10 Cop Event result.
    #[test]
    fn resolve_cop_event_rejects_zero_draw() {
        let mut session = valid_session();
        let err = session
            .execute(ResolveCopEvent::new("m-1", "m-1-a", 0).into_command())
            .expect_err("a zero d10 draw must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A draw beyond the d10 table is not a valid Cop Event result.
    #[test]
    fn resolve_cop_event_rejects_out_of_range_draw() {
        let mut session = valid_session();
        let err = session
            .execute(ResolveCopEvent::new("m-1", "m-1-a", COP_EVENT_DIE_SIDES + 1).into_command())
            .expect_err("a draw beyond the d10 table must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Juice starts at 1 (hard-capped at 10).
    #[test]
    fn resolve_cop_event_rejects_when_starting_juice_is_not_one() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_juice = 3;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_resolve_cop_event().into_command())
            .expect_err("an illegal opening Juice must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn resolve_cop_event_rejects_when_available_juice_exceeds_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.available_juice = JUICE_CAP + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_resolve_cop_event().into_command())
            .expect_err("available Juice over the hard cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a board may hold at most 7 Operators and 3 Vehicles.
    #[test]
    fn resolve_cop_event_rejects_when_board_exceeds_operator_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.operators = MAX_OPERATORS + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_resolve_cop_event().into_command())
            .expect_err("an over-capacity board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn resolve_cop_event_rejects_when_board_exceeds_vehicle_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.vehicles = MAX_VEHICLES + 1;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_resolve_cop_event().into_command())
            .expect_err("an over-capacity vehicle board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Heat is bounded 0..10 and no state may leave it.
    #[test]
    fn resolve_cop_event_rejects_when_heat_leaves_bounds() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_heat = *HEAT_BOUNDS.end() + 1; // Outside [0, 10].
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_resolve_cop_event().into_command())
            .expect_err("Heat outside its bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a Heist resolves only after its prerequisite queue is
    // satisfied.
    #[test]
    fn resolve_cop_event_rejects_when_heist_resolved_with_outstanding_prereqs() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.heist_resolved = true;
        outfit.outstanding_heist_prereqs = 2;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_resolve_cop_event().into_command())
            .expect_err("a Heist resolved with outstanding prereqs must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — drawing from an empty deck deals Fatigue instead of a
    // card, so a match may not carry a deckless Outfit.
    #[test]
    fn resolve_cop_event_rejects_when_deck_is_empty() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.deck_size = 0;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_resolve_cop_event().into_command())
            .expect_err("an empty deck must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a command is valid only for the player whose turn it
    // currently is; a resolution by the off-turn player is rejected.
    #[test]
    fn resolve_cop_event_rejects_when_not_the_players_turn() {
        let mut session = valid_session();
        // Player A holds the turn; player B tries to resolve the Cop Event.
        let err = session
            .execute(ResolveCopEvent::new("m-1", "m-1-b", 7).into_command())
            .expect_err("an off-turn resolution must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A turn-less setup is likewise ill-formed for resolving a Cop Event.
    #[test]
    fn resolve_cop_event_rejects_when_no_opening_player_is_designated() {
        let mut session = valid_session();
        session.set_opening_player(None);

        let err = session
            .execute(valid_resolve_cop_event().into_command())
            .expect_err("a turn-less setup must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a match ends the instant a Boss's HP reaches 0 or
    // below, so a defeated Boss cannot be carried into a Cop Event resolution.
    #[test]
    fn resolve_cop_event_rejects_when_a_boss_is_defeated() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.boss_hp = 0;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_resolve_cop_event().into_command())
            .expect_err("a defeated Boss must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A resolution must name the match this session records.
    #[test]
    fn resolve_cop_event_rejects_when_command_targets_a_different_match() {
        let mut session = valid_session();
        let err = session
            .execute(ResolveCopEvent::new("other-match", "m-1-a", 7).into_command())
            .expect_err("a mismatched match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A resolution must name a configured Outfit.
    #[test]
    fn resolve_cop_event_rejects_unknown_player() {
        let mut session = valid_session();
        let err = session
            .execute(ResolveCopEvent::new("m-1", "ghost", 7).into_command())
            .expect_err("an unknown player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A resolution must identify the player whose Heat triggered the Cop Event.
    #[test]
    fn resolve_cop_event_rejects_blank_player_id() {
        let mut session = valid_session();
        let err = session
            .execute(ResolveCopEvent::new("m-1", "  ", 7).into_command())
            .expect_err("a blank playerId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn resolve_cop_event_command_payload_round_trips() {
        let cmd = valid_resolve_cop_event();
        let command = cmd.into_command();
        assert_eq!(command.name, ResolveCopEvent::COMMAND);
        let decoded: ResolveCopEvent = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_resolve_cop_event());
    }

    // ---- ConcedeMatchCmd (S-9) ---------------------------------------------

    /// A legal `ConcedeMatchCmd` for `m-1`: player `A` forfeits the match. Tests
    /// mutate one aspect at a time to drive a rejection.
    fn valid_concede() -> ConcedeMatch {
        ConcedeMatch::new("m-1", "m-1-a")
    }

    // Scenario: Successfully execute ConcedeMatchCmd.
    #[test]
    fn concedes_match_and_emits_match_completed_event() {
        let mut session = valid_session();

        let events = session
            .execute(valid_concede().into_command())
            .expect("a valid concede should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "match.completed");
        match &events[0] {
            Event::MatchCompleted(done) => {
                assert_eq!(done.match_id, "m-1");
                assert_eq!(done.conceding_player_id, "m-1-a");
                assert_eq!(done.conceding_player, Player::A);
                assert_eq!(done.winning_player_id, "m-1-b");
                assert_eq!(done.winner, Player::B);
            }
            other => panic!("expected MatchCompleted, got {other:?}"),
        }
        assert_eq!(session.version(), 1);
        assert_eq!(session.uncommitted_events().len(), 1);
        assert_eq!(
            session.uncommitted_events()[0].event_type(),
            "match.completed"
        );
    }

    // Concede is the exception to the whose-turn-it-is rule: the player who does
    // *not* hold the turn may still forfeit, handing the win to the turn-holder.
    #[test]
    fn concede_is_allowed_off_turn() {
        let mut session = valid_session(); // player A holds the opening turn.

        // Player B concedes on player A's turn — permitted only for concede.
        let events = session
            .execute(ConcedeMatch::new("m-1", "m-1-b").into_command())
            .expect("conceding off-turn should succeed");

        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::MatchCompleted(done) => {
                assert_eq!(done.conceding_player, Player::B);
                assert_eq!(done.winner, Player::A);
            }
            other => panic!("expected MatchCompleted, got {other:?}"),
        }
    }

    // Scenario: rejected — Juice starts at 1 (hard-capped at 10).
    #[test]
    fn concede_rejects_when_starting_juice_is_not_one() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_juice = 3;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_concede().into_command())
            .expect_err("an illegal opening Juice must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a board may hold at most 7 Operators and 3 Vehicles.
    #[test]
    fn concede_rejects_when_board_exceeds_operator_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.operators = MAX_OPERATORS + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_concede().into_command())
            .expect_err("an over-capacity board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn concede_rejects_when_board_exceeds_vehicle_cap() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.vehicles = MAX_VEHICLES + 1;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_concede().into_command())
            .expect_err("an over-capacity vehicle board must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — Heat is bounded 0..10 and no state may leave it.
    #[test]
    fn concede_rejects_when_heat_leaves_bounds() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.starting_heat = *HEAT_BOUNDS.end() + 1;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_concede().into_command())
            .expect_err("Heat outside its bounds must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a Heist resolves only after its prerequisite queue is
    // satisfied.
    #[test]
    fn concede_rejects_when_heist_resolved_with_outstanding_prereqs() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.heist_resolved = true;
        outfit.outstanding_heist_prereqs = 2;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_concede().into_command())
            .expect_err("a Heist resolved with outstanding prereqs must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — drawing from an empty deck deals Fatigue instead of a
    // card, so a match may not carry a deckless Outfit.
    #[test]
    fn concede_rejects_when_deck_is_empty() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-b");
        outfit.deck_size = 0;
        session.configure_player_b(outfit);

        let err = session
            .execute(valid_concede().into_command())
            .expect_err("an empty deck must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a match ends the instant a Boss's HP reaches 0 or
    // below, so a Boss cannot be conceded around while already defeated.
    #[test]
    fn concede_rejects_when_a_boss_is_already_defeated() {
        let mut session = valid_session();
        let mut outfit = OutfitConfig::new("m-1-a");
        outfit.boss_hp = 0;
        session.configure_player_a(outfit);

        let err = session
            .execute(valid_concede().into_command())
            .expect_err("a defeated Boss must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // Scenario: rejected — a command needs a well-formed, in-progress match; a
    // turn-less session has no whose-turn-it-is and is ill-formed even for
    // concede.
    #[test]
    fn concede_rejects_when_no_opening_player_is_designated() {
        let mut session = valid_session();
        session.set_opening_player(None);

        let err = session
            .execute(valid_concede().into_command())
            .expect_err("a turn-less session must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A concede must name the match this session records.
    #[test]
    fn concede_rejects_when_command_targets_a_different_match() {
        let mut session = valid_session();
        let err = session
            .execute(ConcedeMatch::new("other-match", "m-1-a").into_command())
            .expect_err("a mismatched match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    // A concede must name a configured Outfit.
    #[test]
    fn concede_rejects_unknown_player() {
        let mut session = valid_session();
        let err = session
            .execute(ConcedeMatch::new("m-1", "nobody").into_command())
            .expect_err("an unknown player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn concede_rejects_blank_player() {
        let mut session = valid_session();
        let err = session
            .execute(ConcedeMatch::new("m-1", "   ").into_command())
            .expect_err("a blank playerId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn concede_command_payload_round_trips() {
        let cmd = valid_concede();
        let command = cmd.into_command();
        assert_eq!(command.name, ConcedeMatch::COMMAND);
        let decoded: ConcedeMatch = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_concede());
    }

    // ---- Location-modifier seam / ResolveVenueEventCmd (Task 10) -----------
    //
    // The City pillar's neutral venue modifier: `location: None` (the
    // default) must leave every pre-existing test above green — the hook
    // only does anything when a location is present.

    // Scenario: `LocationModifier::default()` must NOT zero out Heat gain.
    // A derived `Default` would give `heat_multiplier: 0`; the hand-rolled
    // impl fixes it at `1` (a no-op multiplier) while every other field
    // keeps its natural zero/empty default.
    #[test]
    fn location_modifier_default_heat_multiplier_is_one() {
        let loc = LocationModifier::default();
        assert_eq!(loc.heat_multiplier, 1);
        assert_eq!(loc.location_id, "");
        assert_eq!(loc.location_type, "");
        assert!(loc.class_boosts.is_empty());
        assert_eq!(loc.event_table_ref, None);
    }

    #[test]
    fn location_none_is_identity() {
        let session = valid_session(); // location defaults None
        let base = test_unit("u", 3, 3, true, false, &[]);
        let out = session.apply_location_modifiers(Player::A, &base);
        assert_eq!(out, base, "no location => identity");
    }

    #[test]
    fn location_boosts_matching_class_for_both_seats() {
        let mut session = valid_session();
        session.set_location(LocationModifier {
            location_id: "farm-1".into(),
            location_type: "server_farm".into(),
            class_boosts: vec![(CardClass::Hacker, 1)],
            heat_multiplier: 1,
            event_table_ref: None,
        });
        // A Hacker-class unit gets +1 atk regardless of which seat owns it.
        let base = hacker_unit("h", 2, 2);
        assert_eq!(session.apply_location_modifiers(Player::A, &base).atk, 3);
        assert_eq!(session.apply_location_modifiers(Player::B, &base).atk, 3);
    }

    #[test]
    fn location_does_not_boost_non_matching_class() {
        let mut session = valid_session();
        session.set_location(LocationModifier {
            location_id: "farm-1".into(),
            location_type: "server_farm".into(),
            class_boosts: vec![(CardClass::Hacker, 1)],
            heat_multiplier: 1,
            event_table_ref: None,
        });
        // A Neutral-class unit is untouched by a Hacker-only boost.
        let base = test_unit("n", 2, 2, true, false, &[]);
        assert_eq!(session.apply_location_modifiers(Player::A, &base).atk, 2);
    }

    #[test]
    fn location_none_leaves_combat_unaffected() {
        // The consult point in declare_attack is a no-op with location=None.
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        session
            .seat_state_at_mut(Player::A)
            .board
            .push(hacker_unit("A-hacker", 2, 2));
        session
            .seat_state_at_mut(Player::B)
            .board
            .push(test_unit("B-def", 0, 5, true, false, &[]));

        let events = session
            .execute(Attack::new("m-1", "m-1-a", "A-hacker", "op:B-def").into_command())
            .expect("A attacks B's unit");
        assert!(events.iter().any(
            |e| matches!(e, Event::OperatorDamaged(d) if d.instance_id == "B-def" && d.new_hp == 3)
        ));
    }

    // ---- ResolveVenueEventCmd ------------------------------------------

    #[test]
    fn resolve_venue_event_emits_delta_and_is_seeded() {
        let mut session = valid_session();
        let events = session
            .execute(ResolveVenueEvent::new("m-1", "table-noop", 0).into_command())
            .expect("venue event resolves");
        assert!(events
            .iter()
            .any(|e| e.event_type() == "venue.event.resolved"));
    }

    #[test]
    fn resolve_venue_event_rejects_when_command_targets_a_different_match() {
        let mut session = valid_session();
        let err = session
            .execute(ResolveVenueEvent::new("other-match", "table-noop", 0).into_command())
            .expect_err("a mismatched match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn resolve_venue_event_rejects_blank_event_table_ref() {
        let mut session = valid_session();
        let err = session
            .execute(ResolveVenueEvent::new("m-1", "  ", 0).into_command())
            .expect_err("a blank eventTableRef must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(session.version(), 0);
    }

    #[test]
    fn resolve_venue_event_command_payload_round_trips() {
        let cmd = ResolveVenueEvent::new("m-1", "table-noop", 0);
        let command = cmd.into_command();
        assert_eq!(command.name, ResolveVenueEvent::COMMAND);
        let decoded: ResolveVenueEvent = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, ResolveVenueEvent::new("m-1", "table-noop", 0));
    }
}
