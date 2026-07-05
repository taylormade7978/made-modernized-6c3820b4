//! RankedStanding bounded context — a player's competitive rank/rating over a season.
//!
//! A [`RankedStanding`] is one player's competitive record for a season: a
//! hidden Glicko-2 skill estimate (rating, RD, volatility) plus a *visible* rank
//! (a tier and its stars) that climbs the ladder and, crucially, is protected
//! from tilt-driven demotion. Five invariants govern a standing:
//!
//! 1. **Ratings freshness** — the Glicko-2 rating, RD, and volatility are
//!    recalculated after *every* rated match; a standing may not carry a skill
//!    estimate that is stale relative to the matches it has played.
//! 2. **Visible rank shape** — the visible rank advances through tiers
//!    Block→Legend with [`STARS_PER_TIER`] (3) stars per tier; a win streak may
//!    grant at most one *bonus* star, and only while a streak is live.
//! 3. **Rank-floor protection** — protection prevents demotion below a reached
//!    tier floor; this anti-tilt guarantee applies to the lower brackets
//!    (Block/Corner), and a standing may never already sit below its own floor.
//! 4. **Smurf elevation** — a suspected smurf is auto-elevated to a higher
//!    bracket after [`SMURF_ELEVATION_MATCHES`] (20) matches; a suspected smurf
//!    past that threshold that has not been elevated is inconsistent.
//! 5. **Disconnect escalation** — disconnect penalties escalate by *doubling* on
//!    repeated abandonment; the recorded penalty must match the doubling
//!    schedule for the number of abandonments.
//!
//! The only command implemented so far is [`ApplyRankFloorProtection`]
//! (`ApplyRankFloorProtectionCmd`): it pins the visible rank at the reached tier
//! floor so a loss cannot demote the player below it, enforcing every invariant,
//! and on success emits [`Event::RankFloorProtected`] (`ranked.rank.floor.protected`).
//! This module is hand-written (it no longer uses `shared::stub_aggregate!`) but
//! preserves the same public surface — a [`RankedStanding`] aggregate and a
//! [`RankedStandingRepository`] port — so the persistence adapters in
//! `crates/mocks` keep compiling unchanged.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "RankedStanding";

/// The command name [`RankedStanding::execute`] recognizes.
const APPLY_RANK_FLOOR_PROTECTION: &str = "ApplyRankFloorProtectionCmd";

/// Stars per tier: the visible rank advances through each tier with three stars,
/// after which the player promotes to the next tier. A live win streak may add
/// at most one *bonus* star on top of these.
pub const STARS_PER_TIER: u8 = 3;

/// Matches after which a *suspected smurf* is auto-elevated to a higher bracket.
pub const SMURF_ELEVATION_MATCHES: u32 = 20;

/// The base disconnect penalty (in rating points) charged for a first
/// abandonment. Each further abandonment *doubles* the penalty.
pub const BASE_DISCONNECT_PENALTY: u32 = 5;

/// Visible rank tiers, ordered lowest → highest along the competitive ladder.
///
/// The visible rank climbs Block→Legend, three stars at a time. The two lowest
/// tiers — [`Tier::Block`] and [`Tier::Corner`] — are the *anti-tilt* brackets
/// where rank-floor protection is guaranteed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Tier {
    /// Entry bracket; anti-tilt floor protection applies here.
    Block,
    /// Second bracket; the highest tier anti-tilt floor protection applies to.
    Corner,
    /// Mid-ladder bracket.
    Contender,
    /// High-ladder bracket.
    Champion,
    /// Apex bracket.
    Legend,
}

impl Tier {
    /// The tier's position on the ladder (Block = 0 … Legend = 4). Higher is a
    /// stronger rank; used to compare a standing against its reached floor.
    pub fn rank(self) -> u8 {
        match self {
            Tier::Block => 0,
            Tier::Corner => 1,
            Tier::Contender => 2,
            Tier::Champion => 3,
            Tier::Legend => 4,
        }
    }

    /// Whether this tier lies in the anti-tilt brackets (Block/Corner) where
    /// rank-floor protection is guaranteed.
    pub fn is_anti_tilt(self) -> bool {
        self.rank() <= Tier::Corner.rank()
    }

    /// The higher-ranked of two tiers (used to hold the current tier at or above
    /// the reached floor when protection is applied).
    fn max_rank(self, other: Tier) -> Tier {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }
}

/// The `ApplyRankFloorProtectionCmd` payload: the standing to protect and the
/// player it belongs to. Field names are the ladder service's `camelCase`
/// schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ApplyRankFloorProtection::into_command`], or decode it from a command
/// payload via [`serde_json`] inside [`RankedStanding::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRankFloorProtection {
    /// Identity of the standing being protected; must name the standing this
    /// aggregate records.
    pub standing_id: String,
    /// The player this standing belongs to. Must be non-empty and must match the
    /// standing's own player.
    pub player_id: String,
}

impl ApplyRankFloorProtection {
    /// The command name this maps to.
    pub const COMMAND: &'static str = APPLY_RANK_FLOOR_PROTECTION;

    /// Build a command applying rank-floor protection to `standing_id` for
    /// `player_id`.
    pub fn new(standing_id: impl Into<String>, player_id: impl Into<String>) -> Self {
        Self {
            standing_id: standing_id.into(),
            player_id: player_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`RankedStanding::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload =
            serde_json::to_vec(self).expect("ApplyRankFloorProtection is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The protected floor, carried by [`Event::RankFloorProtected`] and thus by the
/// emitted `ranked.rank.floor.protected` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankFloorProtected {
    /// The standing whose floor was pinned.
    pub standing_id: String,
    /// The player the standing belongs to.
    pub player_id: String,
    /// The tier floor a loss can no longer demote the player below.
    pub tier_floor: Tier,
}

/// Domain events emitted by [`RankedStanding`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The visible rank was pinned at the reached tier floor: a subsequent loss
    /// cannot demote the player below it.
    RankFloorProtected(RankFloorProtected),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::RankFloorProtected(_) => "ranked.rank.floor.protected",
        }
    }
}

/// The RankedStanding aggregate: one player's competitive rank/rating for a
/// season.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the server — is unchanged, while it now carries the
/// standing's competitive state: the owning player, the hidden Glicko-2 skill
/// estimate and how fresh it is, the visible tier/stars and reached floor, the
/// match count and smurf status, and the disconnect-penalty ledger. Its
/// `execute` handles [`ApplyRankFloorProtectionCmd`].
///
/// A fresh standing from [`RankedStanding::new`] sits at the ladder's entry
/// (Block, no stars) with a default, freshly-synced Glicko-2 estimate; the
/// configuration methods below drive it to the state a command validates,
/// exactly as [`MatchmakingTicket`](crate::matchmaking_ticket) is built up
/// before a command validates it.
#[derive(Debug)]
pub struct RankedStanding {
    id: String,
    root: AggregateRoot,
    /// The player who owns this standing.
    player_id: String,
    /// Hidden Glicko-2 rating estimate.
    rating: f64,
    /// Hidden Glicko-2 rating deviation (RD).
    rating_deviation: f64,
    /// Hidden Glicko-2 volatility.
    volatility: f64,
    /// Total rated matches this standing has played.
    rated_matches: u32,
    /// The rated-match count at which the Glicko-2 estimate was last
    /// recalculated. Ratings are fresh only when this equals `rated_matches`.
    ratings_synced_at: u32,
    /// Current visible tier.
    tier: Tier,
    /// Stars earned within the current tier (0..=[`STARS_PER_TIER`]).
    stars: u8,
    /// Whether a live win streak has granted the bonus star.
    bonus_star: bool,
    /// Length of the current win streak; a bonus star requires a live streak.
    win_streak: u32,
    /// The highest tier the player has reached — the floor a loss may not demote
    /// them below.
    tier_floor: Tier,
    /// Total matches played (rated or not), used for smurf elevation.
    matches_played: u32,
    /// Whether matchmaking flags this account as a suspected smurf.
    suspected_smurf: bool,
    /// Whether a suspected smurf has been elevated to a higher bracket.
    elevated: bool,
    /// Number of games abandoned via disconnect.
    abandonments: u32,
    /// The currently-charged disconnect penalty (rating points); must follow the
    /// doubling schedule for `abandonments`.
    disconnect_penalty: u32,
}

impl RankedStanding {
    /// Create a new standing identified by `id`, at the ladder entry (Block, no
    /// stars) with a default, freshly-synced Glicko-2 estimate and no penalties.
    /// Use the configuration methods to drive it to the state a command
    /// validates.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            player_id: String::new(),
            // Glicko-2 defaults for an unrated player.
            rating: 1500.0,
            rating_deviation: 350.0,
            volatility: 0.06,
            rated_matches: 0,
            ratings_synced_at: 0,
            tier: Tier::Block,
            stars: 0,
            bonus_star: false,
            win_streak: 0,
            tier_floor: Tier::Block,
            matches_played: 0,
            suspected_smurf: false,
            elevated: false,
            abandonments: 0,
            disconnect_penalty: 0,
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The player who owns this standing.
    pub fn player_id(&self) -> &str {
        &self.player_id
    }

    /// Current visible tier.
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// The reached tier floor a loss may not demote the player below.
    pub fn tier_floor(&self) -> Tier {
        self.tier_floor
    }

    /// The hidden Glicko-2 rating estimate.
    pub fn rating(&self) -> f64 {
        self.rating
    }

    /// The hidden Glicko-2 rating deviation (RD).
    pub fn rating_deviation(&self) -> f64 {
        self.rating_deviation
    }

    /// The hidden Glicko-2 volatility.
    pub fn volatility(&self) -> f64 {
        self.volatility
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// Set the player who owns this standing.
    pub fn set_player(&mut self, player_id: impl Into<String>) {
        self.player_id = player_id.into();
    }

    /// Set the hidden Glicko-2 estimate and how many rated matches it reflects.
    /// Passing `synced_at == rated` marks the estimate fresh.
    pub fn set_ratings(
        &mut self,
        rating: f64,
        rating_deviation: f64,
        volatility: f64,
        rated: u32,
        synced_at: u32,
    ) {
        self.rating = rating;
        self.rating_deviation = rating_deviation;
        self.volatility = volatility;
        self.rated_matches = rated;
        self.ratings_synced_at = synced_at;
    }

    /// Set the visible rank: the tier, its stars, and whether a live win streak
    /// granted the bonus star.
    pub fn set_visible_rank(&mut self, tier: Tier, stars: u8, bonus_star: bool, win_streak: u32) {
        self.tier = tier;
        self.stars = stars;
        self.bonus_star = bonus_star;
        self.win_streak = win_streak;
    }

    /// Set the reached tier floor a loss may not demote the player below.
    pub fn set_tier_floor(&mut self, tier_floor: Tier) {
        self.tier_floor = tier_floor;
    }

    /// Record the smurf-elevation state: total matches, whether the account is a
    /// suspected smurf, and whether it has been elevated.
    pub fn set_smurf_state(&mut self, matches_played: u32, suspected_smurf: bool, elevated: bool) {
        self.matches_played = matches_played;
        self.suspected_smurf = suspected_smurf;
        self.elevated = elevated;
    }

    /// Record the disconnect ledger: number of abandonments and the currently
    /// charged penalty.
    pub fn set_disconnect_ledger(&mut self, abandonments: u32, disconnect_penalty: u32) {
        self.abandonments = abandonments;
        self.disconnect_penalty = disconnect_penalty;
    }

    /// The disconnect penalty owed for `abandonments` losses under the doubling
    /// schedule: `0` for none, then `BASE`, `2·BASE`, `4·BASE`, … Saturates
    /// rather than overflowing on an implausibly long abandonment history.
    fn expected_disconnect_penalty(abandonments: u32) -> u32 {
        match abandonments {
            0 => 0,
            n => BASE_DISCONNECT_PENALTY.saturating_mul(1u32.wrapping_shl(n - 1)),
        }
    }

    /// Ratings-freshness invariant: the Glicko-2 rating, RD, and volatility are
    /// recalculated after every rated match, so the estimate may not lag the
    /// matches played.
    fn ensure_ratings_recalculated(&self) -> Result<(), DomainError> {
        if self.ratings_synced_at != self.rated_matches {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' has a Glicko-2 estimate synced at match {} but has played {} rated \
                 matches; rating, RD, and volatility are recalculated after every rated match",
                self.id, self.ratings_synced_at, self.rated_matches
            )));
        }
        Ok(())
    }

    /// Visible-rank invariant: the rank advances through tiers Block→Legend with
    /// [`STARS_PER_TIER`] stars per tier; a win streak grants at most one bonus
    /// star, and only while a streak is live.
    fn ensure_visible_rank_wellformed(&self) -> Result<(), DomainError> {
        if self.stars > STARS_PER_TIER {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' shows {} stars in tier {:?} but a tier holds at most {} stars \
                 before promotion",
                self.id, self.stars, self.tier, STARS_PER_TIER
            )));
        }
        if self.bonus_star && self.win_streak == 0 {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' carries a bonus star without a live win streak; the bonus star is \
                 granted only by a win streak",
                self.id
            )));
        }
        Ok(())
    }

    /// Rank-floor invariant: rank-floor protection prevents demotion below a
    /// reached tier floor. The anti-tilt guarantee applies to the Block/Corner
    /// brackets, and a standing may never already sit below its own floor.
    fn ensure_floor_protection_valid(&self) -> Result<(), DomainError> {
        if !self.tier_floor.is_anti_tilt() {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' asks to protect a {:?} floor, but rank-floor (anti-tilt) protection \
                 applies only to the Block/Corner brackets",
                self.id, self.tier_floor
            )));
        }
        if self.tier.rank() < self.tier_floor.rank() {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' sits at {:?} below its reached {:?} floor; rank-floor protection \
                 prevents demotion below the floor",
                self.id, self.tier, self.tier_floor
            )));
        }
        Ok(())
    }

    /// Smurf-elevation invariant: a suspected smurf is auto-elevated to a higher
    /// bracket after [`SMURF_ELEVATION_MATCHES`] matches, so a suspected smurf
    /// past that threshold must already be elevated.
    fn ensure_smurf_elevated(&self) -> Result<(), DomainError> {
        if self.suspected_smurf && self.matches_played >= SMURF_ELEVATION_MATCHES && !self.elevated
        {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' is a suspected smurf with {} matches (≥{}) but has not been elevated \
                 to a higher bracket",
                self.id, self.matches_played, SMURF_ELEVATION_MATCHES
            )));
        }
        Ok(())
    }

    /// Disconnect-escalation invariant: penalties escalate by doubling on
    /// repeated abandonment, so the recorded penalty must match the doubling
    /// schedule for the number of abandonments.
    fn ensure_disconnect_penalty_escalates(&self) -> Result<(), DomainError> {
        let expected = Self::expected_disconnect_penalty(self.abandonments);
        if self.disconnect_penalty != expected {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' charges a {}-point disconnect penalty for {} abandonments but the \
                 doubling schedule owes {}; penalties escalate by doubling on repeated abandonment",
                self.id, self.disconnect_penalty, self.abandonments, expected
            )));
        }
        Ok(())
    }

    /// Handle `ApplyRankFloorProtectionCmd`: verify the command targets this
    /// standing and its player, enforce every invariant (ratings freshness,
    /// visible-rank shape, floor validity, smurf elevation, and disconnect
    /// escalation), pin the visible rank at the reached floor, and emit
    /// [`Event::RankFloorProtected`].
    fn apply_rank_floor_protection(
        &mut self,
        cmd: ApplyRankFloorProtection,
    ) -> Result<Vec<Event>, DomainError> {
        // The command must name the standing this aggregate actually records.
        if cmd.standing_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets standing '{}' but this aggregate records '{}'",
                cmd.standing_id, self.id
            )));
        }
        // A valid, matching player must be supplied.
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' requires a valid playerId to apply rank-floor protection",
                self.id
            )));
        }
        if cmd.player_id != self.player_id {
            return Err(DomainError::InvariantViolation(format!(
                "command names player '{}' but standing '{}' belongs to '{}'",
                cmd.player_id, self.id, self.player_id
            )));
        }

        // Enforce every invariant before pinning the floor.
        self.ensure_ratings_recalculated()?;
        self.ensure_visible_rank_wellformed()?;
        self.ensure_floor_protection_valid()?;
        self.ensure_smurf_elevated()?;
        self.ensure_disconnect_penalty_escalates()?;

        let event = Event::RankFloorProtected(RankFloorProtected {
            standing_id: cmd.standing_id,
            player_id: cmd.player_id,
            tier_floor: self.tier_floor,
        });
        // Pin the visible rank at the reached floor: a loss can no longer demote
        // below it, so the current tier is held at or above the floor.
        self.tier = self.tier_floor.max_rank(self.tier);
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for RankedStanding {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            APPLY_RANK_FLOOR_PROTECTION => {
                let cmd: ApplyRankFloorProtection = serde_json::from_slice(&command.payload)
                    .map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ApplyRankFloorProtectionCmd payload: {e}"
                        ))
                    })?;
                self.apply_rank_floor_protection(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`RankedStanding`] aggregate. Adapters implement
/// [`shared::Repository`] for `RankedStanding` and then this marker trait.
pub trait RankedStandingRepository: Repository<RankedStanding> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A protection-ready standing `r-01` for player `p-1`: fresh Glicko-2
    /// estimate, a well-formed visible rank sitting at its reached Corner floor,
    /// no pending smurf elevation, and a disconnect ledger that matches the
    /// doubling schedule. Tests mutate one aspect at a time to drive a specific
    /// rejection.
    fn ready_standing() -> RankedStanding {
        let mut standing = RankedStanding::new("r-01");
        standing.set_player("p-1");
        // Glicko-2 estimate recalculated after all 10 rated matches.
        standing.set_ratings(1620.0, 80.0, 0.059, 10, 10);
        // Two stars in Corner, no bonus star, no live streak.
        standing.set_visible_rank(Tier::Corner, 2, false, 0);
        // Reached floor is Corner (an anti-tilt bracket); current tier is at it.
        standing.set_tier_floor(Tier::Corner);
        // 10 matches, not a suspected smurf.
        standing.set_smurf_state(10, false, false);
        // Two abandonments → BASE·2 = 10 under the doubling schedule.
        standing.set_disconnect_ledger(2, BASE_DISCONNECT_PENALTY * 2);
        standing
    }

    /// A command applying floor protection to `r-01` for player `p-1`.
    fn valid_cmd() -> ApplyRankFloorProtection {
        ApplyRankFloorProtection::new("r-01", "p-1")
    }

    // Scenario: Successfully execute ApplyRankFloorProtectionCmd.
    #[test]
    fn applies_protection_and_emits_rank_floor_protected_event() {
        let mut standing = ready_standing();

        let events = standing
            .execute(valid_cmd().into_command())
            .expect("valid protection should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "ranked.rank.floor.protected");
        match &events[0] {
            Event::RankFloorProtected(protected) => {
                assert_eq!(protected.standing_id, "r-01");
                assert_eq!(protected.player_id, "p-1");
                assert_eq!(protected.tier_floor, Tier::Corner);
            }
        }
        // The standing recorded the event and holds at or above its floor.
        assert!(standing.tier().rank() >= standing.tier_floor().rank());
        assert_eq!(standing.version(), 1);
        assert_eq!(standing.uncommitted_events().len(), 1);
        assert_eq!(
            standing.uncommitted_events()[0].event_type(),
            "ranked.rank.floor.protected"
        );
    }

    // Scenario: rejected — Glicko-2 rating, RD, and volatility are recalculated
    // after every rated match.
    #[test]
    fn rejects_when_ratings_are_stale() {
        let mut standing = ready_standing();
        // The estimate lags: 10 rated matches played, but synced at match 9.
        standing.set_ratings(1620.0, 80.0, 0.059, 10, 9);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("a stale Glicko-2 estimate must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — visible rank advances through tiers Block→Legend with
    // 3 stars per tier; a win streak grants a bonus star.
    #[test]
    fn rejects_when_visible_rank_exceeds_stars_per_tier() {
        let mut standing = ready_standing();
        // Four stars in a tier that promotes at three is malformed.
        standing.set_visible_rank(Tier::Corner, STARS_PER_TIER + 1, false, 0);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("a tier over its star cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Visible-rank invariant: a bonus star requires a live win streak.
    #[test]
    fn rejects_bonus_star_without_a_win_streak() {
        let mut standing = ready_standing();
        // Bonus star claimed with no live streak.
        standing.set_visible_rank(Tier::Corner, 2, true, 0);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("a bonus star without a streak must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — rank-floor protection prevents demotion below a
    // reached tier floor (anti-tilt applies to Block/Corner).
    #[test]
    fn rejects_when_standing_is_below_its_floor() {
        let mut standing = ready_standing();
        // Current tier Block sits below the reached Corner floor.
        standing.set_visible_rank(Tier::Block, 2, false, 0);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("a standing below its floor must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Rank-floor invariant: anti-tilt protection applies only to Block/Corner.
    #[test]
    fn rejects_floor_protection_above_anti_tilt_brackets() {
        let mut standing = ready_standing();
        // A Champion floor is above the Block/Corner anti-tilt brackets.
        standing.set_visible_rank(Tier::Champion, 2, false, 0);
        standing.set_tier_floor(Tier::Champion);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("protecting a floor above Corner must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — a suspected smurf is auto-elevated to a higher bracket
    // after 20 matches.
    #[test]
    fn rejects_suspected_smurf_not_yet_elevated() {
        let mut standing = ready_standing();
        // A suspected smurf past the 20-match threshold that was not elevated.
        standing.set_smurf_state(SMURF_ELEVATION_MATCHES, true, false);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("an un-elevated suspected smurf must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — disconnect penalties escalate (doubling) on repeated
    // abandonment.
    #[test]
    fn rejects_when_disconnect_penalty_does_not_double() {
        let mut standing = ready_standing();
        // Three abandonments owe BASE·4 under doubling; charging BASE·3 breaks it.
        standing.set_disconnect_ledger(3, BASE_DISCONNECT_PENALTY * 3);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("a penalty off the doubling schedule must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A command naming a different standing is rejected before any invariant runs.
    #[test]
    fn rejects_command_for_a_different_standing() {
        let mut standing = ready_standing();
        let cmd = ApplyRankFloorProtection::new("r-99", "p-1");

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a command for another standing must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A command for the wrong player is rejected.
    #[test]
    fn rejects_command_for_a_different_player() {
        let mut standing = ready_standing();
        let cmd = ApplyRankFloorProtection::new("r-01", "p-other");

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a command for another player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A command with no player is rejected.
    #[test]
    fn rejects_command_without_a_player() {
        let mut standing = ready_standing();
        let cmd = ApplyRankFloorProtection::new("r-01", "   ");

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a missing playerId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut standing = RankedStanding::new("r-01");
        let err = standing.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "RankedStanding");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, ApplyRankFloorProtection::COMMAND);
        let decoded: ApplyRankFloorProtection = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }

    #[test]
    fn disconnect_schedule_doubles() {
        assert_eq!(RankedStanding::expected_disconnect_penalty(0), 0);
        assert_eq!(
            RankedStanding::expected_disconnect_penalty(1),
            BASE_DISCONNECT_PENALTY
        );
        assert_eq!(
            RankedStanding::expected_disconnect_penalty(2),
            BASE_DISCONNECT_PENALTY * 2
        );
        assert_eq!(
            RankedStanding::expected_disconnect_penalty(3),
            BASE_DISCONNECT_PENALTY * 4
        );
    }
}
