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
//! The only command implemented so far is [`StartMatch`] (`StartMatchCmd`): it
//! initializes a session from two [`OutfitConfig`]s, an RNG seed, and each
//! Outfit's Boss, enforcing the match-play rules-contract invariants up front,
//! and on success emits [`Event::MatchStarted`] (`match.started`). The module is
//! hand-written (it no longer uses `shared::stub_aggregate!`) but preserves the
//! same public surface — a [`GameSession`] aggregate and a
//! [`GameSessionRepository`] port — so the persistence adapters in
//! `crates/mocks` and the actix-web server keep compiling unchanged.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "GameSession";

/// The command name [`GameSession::execute`] recognizes.
const START_MATCH: &str = "StartMatchCmd";

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

/// Domain events emitted by [`GameSession`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A match passed every start-time invariant and was initialized.
    MatchStarted(MatchStarted),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::MatchStarted(_) => "match.started",
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
}
