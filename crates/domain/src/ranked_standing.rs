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
//! Four commands are implemented. [`ApplyRankFloorProtection`]
//! (`ApplyRankFloorProtectionCmd`) pins the visible rank at the reached tier
//! floor so a loss cannot demote the player below it, enforcing every invariant,
//! and on success emits [`Event::RankFloorProtected`] (`ranked.rank.floor.protected`).
//! [`ElevateSmurf`] (`ElevateSmurfCmd`) auto-promotes a detected smurf — a
//! suspected smurf that has reached [`SMURF_ELEVATION_MATCHES`] (20) matches —
//! into the next bracket, enforcing the same rest invariants, and on success
//! emits [`Event::SmurfElevated`] (`smurf.elevated`).
//! [`RecordMatchResult`] (`RecordMatchResultCmd`) ingests a completed rated match
//! and recomputes the hidden Glicko-2 estimate (rating, RD, volatility) from the
//! outcome and the opponent's rating — honoring invariant 1, that ratings are
//! recalculated after *every* rated match — and on success emits both
//! [`Event::MatchResultRecorded`] (`match.result.recorded`) and
//! [`Event::RatingRecalculated`] (`rating.recalculated`).
//! [`ApplyDisconnectPenalty`] (`ApplyDisconnectPenaltyCmd`) charges an escalating
//! penalty for an abandonment — doubling the prior charge — enforcing every
//! invariant (the disconnect-escalation invariant confirming the existing ledger
//! is consistent before the schedule advances), and on success emits
//! [`Event::DisconnectPenaltyApplied`] (`disconnect.penalty.applied`).
//! [`AwardStar`] (`AwardStarCmd`) grants a star (plus a streak bonus) on a win,
//! enforcing the same invariants; it emits [`Event::StarAwarded`]
//! (`star.awarded`) and, when the awarded stars fill the tier, also
//! promotes the visible rank and emits [`Event::RankPromoted`]
//! (`rank.promoted`).
//! This module is hand-written (it no longer uses `shared::stub_aggregate!`) but
//! preserves the same public surface — a [`RankedStanding`] aggregate and a
//! [`RankedStandingRepository`] port — so the persistence adapters in
//! `crates/mocks` keep compiling unchanged.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "RankedStanding";

/// The command name [`RankedStanding::execute`] recognizes for pinning a floor.
const APPLY_RANK_FLOOR_PROTECTION: &str = "ApplyRankFloorProtectionCmd";

/// The command name [`RankedStanding::execute`] recognizes for auto-elevating a
/// detected smurf to a higher bracket.
const ELEVATE_SMURF: &str = "ElevateSmurfCmd";

/// The command name [`RankedStanding::execute`] recognizes for charging an
/// escalating disconnect penalty for an abandonment.
const APPLY_DISCONNECT_PENALTY: &str = "ApplyDisconnectPenaltyCmd";

/// The `RecordMatchResultCmd` name [`RankedStanding::execute`] recognizes.
const RECORD_MATCH_RESULT: &str = "RecordMatchResultCmd";

/// The command name [`RankedStanding::execute`] recognizes for granting a star
/// (plus an optional streak bonus) on a win.
const AWARD_STAR: &str = "AwardStarCmd";

/// Stars per tier: the visible rank advances through each tier with three stars,
/// after which the player promotes to the next tier. A live win streak may add
/// at most one *bonus* star on top of these.
pub const STARS_PER_TIER: u8 = 3;

/// Matches after which a *suspected smurf* is auto-elevated to a higher bracket.
pub const SMURF_ELEVATION_MATCHES: u32 = 20;

/// The base disconnect penalty (in rating points) charged for a first
/// abandonment. Each further abandonment *doubles* the penalty.
pub const BASE_DISCONNECT_PENALTY: u32 = 5;

/// Glicko-2 scale factor: the constant `173.7178` that maps the human-readable
/// rating scale (centered at 1500) to and from the internal Glicko-2 scale on
/// which the update is computed.
const GLICKO2_SCALE: f64 = 173.7178;

/// Glicko-2 system constant `τ` (tau), constraining how much volatility may move
/// in a single update. `0.5` is the value the Glicko-2 paper recommends for most
/// competitive settings.
const GLICKO2_TAU: f64 = 0.5;

/// Assumed rating deviation (RD) of the opponent for a single rated match. A
/// `RecordMatchResultCmd` carries only the opponent's rating, so the update
/// treats the opponent as reasonably well-established at this RD.
const ASSUMED_OPPONENT_RD: f64 = 50.0;

/// Convergence tolerance for the Illinois root-finding iteration that solves for
/// the new Glicko-2 volatility.
const GLICKO2_CONVERGENCE: f64 = 1e-6;

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

    /// The next tier up the ladder, or `None` at the apex ([`Tier::Legend`]).
    /// Used to promote a standing when a star fills the current tier.
    fn next(self) -> Option<Tier> {
        match self {
            Tier::Block => Some(Tier::Corner),
            Tier::Corner => Some(Tier::Contender),
            Tier::Contender => Some(Tier::Champion),
            Tier::Champion => Some(Tier::Legend),
            Tier::Legend => None,
        }
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

    /// The next bracket up the ladder — the tier a suspected smurf is elevated
    /// into. [`Tier::Legend`], already the apex, elevates to itself.
    fn elevated_bracket(self) -> Tier {
        match self {
            Tier::Block => Tier::Corner,
            Tier::Corner => Tier::Contender,
            Tier::Contender => Tier::Champion,
            Tier::Champion => Tier::Legend,
            Tier::Legend => Tier::Legend,
        }
    }
}

/// The outcome of a completed rated match, from the perspective of the standing
/// whose rating is being recomputed. Maps to the Glicko-2 score `s`: a win
/// scores `1.0`, a draw `0.5`, and a loss `0.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum MatchOutcome {
    /// The player won the match.
    Win,
    /// The match was drawn.
    Draw,
    /// The player lost the match.
    Loss,
}

impl MatchOutcome {
    /// The Glicko-2 score `s` for this outcome: `1.0` win, `0.5` draw, `0.0`
    /// loss.
    fn score(self) -> f64 {
        match self {
            MatchOutcome::Win => 1.0,
            MatchOutcome::Draw => 0.5,
            MatchOutcome::Loss => 0.0,
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

/// The `AwardStarCmd` payload: the standing to credit, the player it belongs to,
/// and whether a live win streak grants a bonus star. Field names are the ladder
/// service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`AwardStar::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`RankedStanding::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwardStar {
    /// Identity of the standing being credited; must name the standing this
    /// aggregate records.
    pub standing_id: String,
    /// The player this standing belongs to. Must be non-empty and must match the
    /// standing's own player.
    pub player_id: String,
    /// Whether the win extends a streak and so grants an extra *bonus* star on top
    /// of the base star. Only valid while a win streak is live.
    pub streak_bonus: bool,
}

impl AwardStar {
    /// The command name this maps to.
    pub const COMMAND: &'static str = AWARD_STAR;

    /// Build a command awarding a star to `standing_id` for `player_id`, granting
    /// a bonus star when `streak_bonus` is set.
    pub fn new(
        standing_id: impl Into<String>,
        player_id: impl Into<String>,
        streak_bonus: bool,
    ) -> Self {
        Self {
            standing_id: standing_id.into(),
            player_id: player_id.into(),
            streak_bonus,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`RankedStanding::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("AwardStar is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `ElevateSmurfCmd` payload: the standing to elevate, its player, and the
/// match count that tripped smurf detection. Field names are the ladder
/// service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ElevateSmurf::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`RankedStanding::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElevateSmurf {
    /// Identity of the standing being elevated; must name the standing this
    /// aggregate records.
    pub standing_id: String,
    /// The player this standing belongs to. Must be non-empty and must match the
    /// standing's own player.
    pub player_id: String,
    /// The observed match count that tripped smurf detection. Must match the
    /// standing's recorded matches played, and reach [`SMURF_ELEVATION_MATCHES`].
    pub match_count: u32,
}

impl ElevateSmurf {
    /// The command name this maps to.
    pub const COMMAND: &'static str = ELEVATE_SMURF;

    /// Build a command elevating `standing_id` (for `player_id`) after
    /// `match_count` matches.
    pub fn new(
        standing_id: impl Into<String>,
        player_id: impl Into<String>,
        match_count: u32,
    ) -> Self {
        Self {
            standing_id: standing_id.into(),
            player_id: player_id.into(),
            match_count,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`RankedStanding::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ElevateSmurf is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `RecordMatchResultCmd` payload: a completed rated match to ingest into a
/// standing. Field names are the ladder service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`RecordMatchResult::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`RankedStanding::execute`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordMatchResult {
    /// Identity of the standing recording the result; must name the standing
    /// this aggregate records.
    pub standing_id: String,
    /// The player this standing belongs to. Must be non-empty and must match the
    /// standing's own player.
    pub player_id: String,
    /// Identity of the completed match. Must be non-empty.
    pub match_id: String,
    /// The outcome of the match for this player (win / draw / loss).
    pub outcome: MatchOutcome,
    /// The opponent's (human-scale) Glicko-2 rating. Must be a finite, positive
    /// number so the recompute is well-defined.
    pub opponent_rating: f64,
}

impl RecordMatchResult {
    /// The command name this maps to.
    pub const COMMAND: &'static str = RECORD_MATCH_RESULT;

    /// Build a command recording `outcome` against an opponent rated
    /// `opponent_rating` for `match_id`, on `standing_id` for `player_id`.
    pub fn new(
        standing_id: impl Into<String>,
        player_id: impl Into<String>,
        match_id: impl Into<String>,
        outcome: MatchOutcome,
        opponent_rating: f64,
    ) -> Self {
        Self {
            standing_id: standing_id.into(),
            player_id: player_id.into(),
            match_id: match_id.into(),
            outcome,
            opponent_rating,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`RankedStanding::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("RecordMatchResult is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `ApplyDisconnectPenaltyCmd` payload: the standing to penalize, its player,
/// and the count of penalties already charged. Field names are the ladder
/// service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ApplyDisconnectPenalty::into_command`], or decode it from a command payload
/// via [`serde_json`] inside [`RankedStanding::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyDisconnectPenalty {
    /// Identity of the standing being penalized; must name the standing this
    /// aggregate records.
    pub standing_id: String,
    /// The player this standing belongs to. Must be non-empty and must match the
    /// standing's own player.
    pub player_id: String,
    /// The number of disconnect penalties already charged (i.e. prior
    /// abandonments). Must agree with the standing's recorded abandonment count,
    /// so the escalation continues the doubling schedule from the right place.
    pub prior_penalties: u32,
}

impl ApplyDisconnectPenalty {
    /// The command name this maps to.
    pub const COMMAND: &'static str = APPLY_DISCONNECT_PENALTY;

    /// Build a command charging a disconnect penalty to `standing_id` (for
    /// `player_id`) after `prior_penalties` prior abandonments.
    pub fn new(
        standing_id: impl Into<String>,
        player_id: impl Into<String>,
        prior_penalties: u32,
    ) -> Self {
        Self {
            standing_id: standing_id.into(),
            player_id: player_id.into(),
            prior_penalties,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`RankedStanding::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload =
            serde_json::to_vec(self).expect("ApplyDisconnectPenalty is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The elevation, carried by [`Event::SmurfElevated`] and thus by the emitted
/// `smurf.elevated` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmurfElevated {
    /// The standing that was elevated.
    pub standing_id: String,
    /// The player the standing belongs to.
    pub player_id: String,
    /// The match count that tripped smurf detection.
    pub match_count: u32,
    /// The higher bracket the suspected smurf was elevated into.
    pub elevated_to: Tier,
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

/// A star (and, when a streak is live, a bonus star) awarded on a win; carried by
/// [`Event::StarAwarded`] and thus by the emitted `star.awarded` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarAwarded {
    /// The standing credited with the star(s).
    pub standing_id: String,
    /// The player the standing belongs to.
    pub player_id: String,
    /// The tier the star was awarded in.
    pub tier: Tier,
    /// How many stars this win granted: one, or two with a streak bonus.
    pub stars_awarded: u8,
    /// Whether a streak bonus star was included.
    pub streak_bonus: bool,
}

/// A promotion to the next tier, triggered when awarded stars fill the current
/// tier; carried by [`Event::RankPromoted`] and thus by the emitted
/// `rank.promoted` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankPromoted {
    /// The standing that was promoted.
    pub standing_id: String,
    /// The player the standing belongs to.
    pub player_id: String,
    /// The tier the player promoted out of.
    pub from_tier: Tier,
    /// The tier the player promoted into.
    pub to_tier: Tier,
}

/// The charged penalty, carried by [`Event::DisconnectPenaltyApplied`] and thus
/// by the emitted `disconnect.penalty.applied` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectPenaltyApplied {
    /// The standing that was penalized.
    pub standing_id: String,
    /// The player the standing belongs to.
    pub player_id: String,
    /// The number of penalties charged before this one.
    pub prior_penalties: u32,
    /// The penalty (rating points) charged for this abandonment, doubled from the
    /// prior charge under the escalation schedule.
    pub penalty: u32,
    /// The standing's abandonment count after recording this penalty.
    pub abandonments: u32,
}

/// A completed rated match ingested into a standing, carried by
/// [`Event::MatchResultRecorded`] and thus by the emitted `match.result.recorded`
/// event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResultRecorded {
    /// The standing the result was recorded on.
    pub standing_id: String,
    /// The player the standing belongs to.
    pub player_id: String,
    /// The completed match this result came from.
    pub match_id: String,
    /// The outcome recorded for the player.
    pub outcome: MatchOutcome,
}

/// The recomputed Glicko-2 estimate, carried by [`Event::RatingRecalculated`]
/// and thus by the emitted `rating.recalculated` event.
#[derive(Debug, Clone, PartialEq)]
pub struct RatingRecalculated {
    /// The standing whose rating was recomputed.
    pub standing_id: String,
    /// The player the standing belongs to.
    pub player_id: String,
    /// The new hidden Glicko-2 rating.
    pub rating: f64,
    /// The new hidden Glicko-2 rating deviation (RD).
    pub rating_deviation: f64,
    /// The new hidden Glicko-2 volatility.
    pub volatility: f64,
    /// The rated-match count the new estimate reflects (it is now fresh).
    pub rated_matches: u32,
}

/// Domain events emitted by [`RankedStanding`].
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The visible rank was pinned at the reached tier floor: a subsequent loss
    /// cannot demote the player below it.
    RankFloorProtected(RankFloorProtected),
    /// A star (plus optional streak bonus) was awarded on a win.
    StarAwarded(StarAwarded),
    /// Awarded stars filled the tier and the visible rank promoted upward.
    RankPromoted(RankPromoted),
    /// A suspected smurf was auto-elevated to a higher bracket after reaching the
    /// match threshold.
    SmurfElevated(SmurfElevated),
    /// An escalating disconnect penalty was charged for an abandonment.
    DisconnectPenaltyApplied(DisconnectPenaltyApplied),
    /// A completed rated match was recorded on the standing.
    MatchResultRecorded(MatchResultRecorded),
    /// The hidden Glicko-2 estimate (rating, RD, volatility) was recomputed.
    RatingRecalculated(RatingRecalculated),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::RankFloorProtected(_) => "ranked.rank.floor.protected",
            Event::StarAwarded(_) => "star.awarded",
            Event::RankPromoted(_) => "rank.promoted",
            Event::SmurfElevated(_) => "smurf.elevated",
            Event::DisconnectPenaltyApplied(_) => "disconnect.penalty.applied",
            Event::MatchResultRecorded(_) => "match.result.recorded",
            Event::RatingRecalculated(_) => "rating.recalculated",
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
/// `execute` handles ranked-standing commands.
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

    /// Smurf-elevation eligibility: a standing may be elevated only when it is a
    /// *suspected smurf* that has reached [`SMURF_ELEVATION_MATCHES`] matches and
    /// has not already been elevated. This is the transition that *establishes*
    /// the smurf-elevation rest invariant ([`Self::ensure_smurf_elevated`]),
    /// which is why `ElevateSmurfCmd` gates on eligibility rather than on that
    /// rest invariant (which by construction the pre-elevation state violates).
    fn ensure_smurf_elevation_warranted(&self) -> Result<(), DomainError> {
        if self.elevated {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' has already been elevated to a higher bracket; a smurf is elevated \
                 at most once",
                self.id
            )));
        }
        if !(self.suspected_smurf && self.matches_played >= SMURF_ELEVATION_MATCHES) {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' is not eligible for smurf elevation: only a suspected smurf is \
                 auto-elevated to a higher bracket, and only after {} matches (has {}, suspected: \
                 {})",
                self.id, SMURF_ELEVATION_MATCHES, self.matches_played, self.suspected_smurf
            )));
        }
        Ok(())
    }

    /// Handle `ElevateSmurfCmd`: verify the command targets this standing and its
    /// player and carries a match count consistent with the standing's record,
    /// enforce the standing's rest invariants (ratings freshness, visible-rank
    /// shape, floor validity, and disconnect escalation), confirm elevation is
    /// warranted, promote the standing to the next bracket, and emit
    /// [`Event::SmurfElevated`].
    fn elevate_smurf(&mut self, cmd: ElevateSmurf) -> Result<Vec<Event>, DomainError> {
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
                "standing '{}' requires a valid playerId to elevate a smurf",
                self.id
            )));
        }
        if cmd.player_id != self.player_id {
            return Err(DomainError::InvariantViolation(format!(
                "command names player '{}' but standing '{}' belongs to '{}'",
                cmd.player_id, self.id, self.player_id
            )));
        }
        // A valid matchCount must be supplied: it must agree with the matches the
        // standing has actually recorded.
        if cmd.match_count != self.matches_played {
            return Err(DomainError::InvariantViolation(format!(
                "command reports {} matches but standing '{}' has recorded {}; the elevation match \
                 count must match the standing's record",
                cmd.match_count, self.id, self.matches_played
            )));
        }

        // Enforce the rest invariants that hold independently of elevation.
        self.ensure_ratings_recalculated()?;
        self.ensure_visible_rank_wellformed()?;
        self.ensure_floor_protection_valid()?;
        self.ensure_disconnect_penalty_escalates()?;
        // Elevation is warranted only for an un-elevated, eligible suspected smurf.
        self.ensure_smurf_elevation_warranted()?;

        let elevated_to = self.tier.elevated_bracket();
        let event = Event::SmurfElevated(SmurfElevated {
            standing_id: cmd.standing_id,
            player_id: cmd.player_id,
            match_count: cmd.match_count,
            elevated_to,
        });
        // Promote to the higher bracket and record that this smurf is now elevated.
        self.tier = elevated_to;
        self.elevated = true;
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
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

    /// Handle `AwardStarCmd`: verify the command targets this standing and its
    /// player, enforce every invariant, then grant a star (plus a streak bonus
    /// when one is claimed and a streak is live). Emits
    /// [`Event::StarAwarded`]; when the awarded stars fill the tier
    /// ([`STARS_PER_TIER`]) the visible rank promotes to the next tier and a
    /// second [`Event::RankPromoted`] is emitted.
    fn award_star(&mut self, cmd: AwardStar) -> Result<Vec<Event>, DomainError> {
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
                "standing '{}' requires a valid playerId to award a star",
                self.id
            )));
        }
        if cmd.player_id != self.player_id {
            return Err(DomainError::InvariantViolation(format!(
                "command names player '{}' but standing '{}' belongs to '{}'",
                cmd.player_id, self.id, self.player_id
            )));
        }
        // A streak bonus may be claimed only while a win streak is live; awarding
        // it otherwise would violate the visible-rank (bonus-star) invariant.
        if cmd.streak_bonus && self.win_streak == 0 {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' claims a streak bonus star without a live win streak; the bonus \
                 star is granted only by a win streak",
                self.id
            )));
        }

        // Enforce every invariant against the pre-award state before crediting.
        self.ensure_ratings_recalculated()?;
        self.ensure_visible_rank_wellformed()?;
        self.ensure_floor_protection_valid()?;
        self.ensure_smurf_elevated()?;
        self.ensure_disconnect_penalty_escalates()?;

        // One base star, plus a bonus star when the win extends a live streak.
        let stars_awarded = 1 + u8::from(cmd.streak_bonus);
        let total = self.stars + stars_awarded;

        let mut events = Vec::new();
        let awarded = Event::StarAwarded(StarAwarded {
            standing_id: cmd.standing_id.clone(),
            player_id: cmd.player_id.clone(),
            tier: self.tier,
            stars_awarded,
            streak_bonus: cmd.streak_bonus,
        });
        events.push(awarded);

        // Filling the tier's star cap promotes to the next tier (if one exists),
        // carrying any surplus stars into it and raising the anti-tilt floor.
        if total >= STARS_PER_TIER {
            if let Some(next) = self.tier.next() {
                let from_tier = self.tier;
                self.tier = next;
                self.stars = total - STARS_PER_TIER;
                self.bonus_star = false;
                // A newly-reached anti-tilt bracket becomes the protected floor.
                if next.is_anti_tilt() {
                    self.tier_floor = self.tier_floor.max_rank(next);
                }
                events.push(Event::RankPromoted(RankPromoted {
                    standing_id: cmd.standing_id,
                    player_id: cmd.player_id,
                    from_tier,
                    to_tier: next,
                }));
            } else {
                // Already at the apex: hold at the star cap, no promotion.
                self.stars = STARS_PER_TIER;
            }
        } else {
            self.stars = total;
            self.bonus_star = cmd.streak_bonus;
        }

        for event in &events {
            self.root.record(Box::new(event.clone()));
        }
        Ok(events)
    }

    /// Handle `ApplyDisconnectPenaltyCmd`: verify the command targets this
    /// standing and its player and carries a prior-penalty count consistent with
    /// the standing's record, enforce every invariant (ratings freshness,
    /// visible-rank shape, floor validity, smurf elevation, and — on the existing
    /// ledger — disconnect escalation), charge the next (doubled) penalty by
    /// recording one more abandonment, and emit
    /// [`Event::DisconnectPenaltyApplied`].
    fn apply_disconnect_penalty(
        &mut self,
        cmd: ApplyDisconnectPenalty,
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
                "standing '{}' requires a valid playerId to apply a disconnect penalty",
                self.id
            )));
        }
        if cmd.player_id != self.player_id {
            return Err(DomainError::InvariantViolation(format!(
                "command names player '{}' but standing '{}' belongs to '{}'",
                cmd.player_id, self.id, self.player_id
            )));
        }
        // A valid priorPenalties must be supplied: it must agree with the
        // abandonments the standing has actually recorded, so the escalation
        // continues the doubling schedule from the right place.
        if cmd.prior_penalties != self.abandonments {
            return Err(DomainError::InvariantViolation(format!(
                "command reports {} prior penalties but standing '{}' has recorded {} \
                 abandonments; the prior-penalty count must match the standing's record",
                cmd.prior_penalties, self.id, self.abandonments
            )));
        }

        // Enforce every invariant before charging the penalty; the disconnect
        // escalation invariant confirms the *existing* ledger is consistent under
        // doubling so the next charge continues the schedule.
        self.ensure_ratings_recalculated()?;
        self.ensure_visible_rank_wellformed()?;
        self.ensure_floor_protection_valid()?;
        self.ensure_smurf_elevated()?;
        self.ensure_disconnect_penalty_escalates()?;

        // Charge the next abandonment: the penalty doubles from the prior charge.
        let abandonments = self.abandonments.saturating_add(1);
        let penalty = Self::expected_disconnect_penalty(abandonments);
        let event = Event::DisconnectPenaltyApplied(DisconnectPenaltyApplied {
            standing_id: cmd.standing_id,
            player_id: cmd.player_id,
            prior_penalties: cmd.prior_penalties,
            penalty,
            abandonments,
        });
        // Record the escalated penalty, keeping the ledger on the doubling schedule.
        self.abandonments = abandonments;
        self.disconnect_penalty = penalty;
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Solve the Glicko-2 volatility update equation for the new volatility using
    /// the Illinois variant of regula-falsi, exactly as specified in Glickman's
    /// Glicko-2 paper. `phi` is the pre-update deviation, `v` the estimated
    /// variance, and `delta` the estimated rating change, all on the Glicko-2
    /// scale.
    fn recompute_volatility(sigma: f64, phi: f64, v: f64, delta: f64) -> f64 {
        let tau = GLICKO2_TAU;
        let a = (sigma * sigma).ln();
        // f(x) whose root (in x = ln σ'²) gives the new volatility.
        let f = |x: f64| {
            let ex = x.exp();
            let num = ex * (delta * delta - phi * phi - v - ex);
            let den = 2.0 * (phi * phi + v + ex).powi(2);
            num / den - (x - a) / (tau * tau)
        };

        // Bracket the root: A below, B above.
        let mut big_a = a;
        let mut big_b = if delta * delta > phi * phi + v {
            (delta * delta - phi * phi - v).ln()
        } else {
            let mut k = 1.0;
            while f(a - k * tau) < 0.0 {
                k += 1.0;
            }
            a - k * tau
        };

        let mut f_a = f(big_a);
        let mut f_b = f(big_b);
        while (big_b - big_a).abs() > GLICKO2_CONVERGENCE {
            let c = big_a + (big_a - big_b) * f_a / (f_b - f_a);
            let f_c = f(c);
            if f_c * f_b <= 0.0 {
                big_a = big_b;
                f_a = f_b;
            } else {
                f_a /= 2.0;
            }
            big_b = c;
            f_b = f_c;
        }

        (big_a / 2.0).exp()
    }

    /// Run one Glicko-2 rating period against a single opponent, returning the new
    /// `(rating, rating_deviation, volatility)` on the human-readable scale.
    ///
    /// Implements the standard Glicko-2 update (Glickman, 2013): convert to the
    /// internal scale, weigh the opponent via `g(φ)`/`E`, solve for the new
    /// volatility, contract the deviation, shift the rating, and convert back.
    fn recompute_rating(&self, outcome: MatchOutcome, opponent_rating: f64) -> (f64, f64, f64) {
        // Step 2: onto the Glicko-2 scale (μ, φ), centered at 1500.
        let mu = (self.rating - 1500.0) / GLICKO2_SCALE;
        let phi = self.rating_deviation / GLICKO2_SCALE;
        let mu_j = (opponent_rating - 1500.0) / GLICKO2_SCALE;
        let phi_j = ASSUMED_OPPONENT_RD / GLICKO2_SCALE;

        // Step 3: g(φ) dampens by opponent uncertainty; E is the expected score.
        let g = 1.0
            / (1.0 + 3.0 * phi_j * phi_j / (std::f64::consts::PI * std::f64::consts::PI)).sqrt();
        let e = 1.0 / (1.0 + (-g * (mu - mu_j)).exp());
        let s = outcome.score();

        // Step 4/5: estimated variance v and rating change delta.
        let v = 1.0 / (g * g * e * (1.0 - e));
        let delta = v * g * (s - e);

        // Step 6: new volatility via the iterative solver.
        let sigma_prime = Self::recompute_volatility(self.volatility, phi, v, delta);

        // Step 7/8: pre-rating-period deviation, then contract with new evidence.
        let phi_star = (phi * phi + sigma_prime * sigma_prime).sqrt();
        let phi_prime = 1.0 / (1.0 / (phi_star * phi_star) + 1.0 / v).sqrt();

        // Step 8: shift the rating by the observed-minus-expected score.
        let mu_prime = mu + phi_prime * phi_prime * g * (s - e);

        // Step 9: back to the human-readable scale.
        (
            GLICKO2_SCALE * mu_prime + 1500.0,
            GLICKO2_SCALE * phi_prime,
            sigma_prime,
        )
    }

    /// Handle `RecordMatchResultCmd`: verify the command targets this standing and
    /// its player with a valid match and opponent rating, enforce every invariant
    /// as a precondition (ratings freshness, visible-rank shape, floor validity,
    /// smurf elevation, and disconnect escalation), ingest the rated match,
    /// recompute the hidden Glicko-2 estimate, and emit both
    /// [`Event::MatchResultRecorded`] and [`Event::RatingRecalculated`].
    fn record_match_result(&mut self, cmd: RecordMatchResult) -> Result<Vec<Event>, DomainError> {
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
                "standing '{}' requires a valid playerId to record a match result",
                self.id
            )));
        }
        if cmd.player_id != self.player_id {
            return Err(DomainError::InvariantViolation(format!(
                "command names player '{}' but standing '{}' belongs to '{}'",
                cmd.player_id, self.id, self.player_id
            )));
        }
        // A valid match identity and opponent rating must be supplied.
        if cmd.match_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' requires a valid matchId to record a match result",
                self.id
            )));
        }
        if !cmd.opponent_rating.is_finite() || cmd.opponent_rating <= 0.0 {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' requires a finite, positive opponentRating but got {}",
                self.id, cmd.opponent_rating
            )));
        }

        // Enforce every invariant on the standing's state *before* recording the
        // new match — the estimate being ingested must itself be consistent.
        self.ensure_ratings_recalculated()?;
        self.ensure_visible_rank_wellformed()?;
        self.ensure_floor_protection_valid()?;
        self.ensure_smurf_elevated()?;
        self.ensure_disconnect_penalty_escalates()?;

        // Recompute the Glicko-2 estimate from the outcome and opponent rating.
        let (rating, rating_deviation, volatility) =
            self.recompute_rating(cmd.outcome, cmd.opponent_rating);

        // Ingest the rated match: the counters advance and the fresh estimate is
        // synced to the new count, upholding the ratings-freshness invariant.
        self.rated_matches += 1;
        self.matches_played += 1;
        self.rating = rating;
        self.rating_deviation = rating_deviation;
        self.volatility = volatility;
        self.ratings_synced_at = self.rated_matches;

        let recorded = Event::MatchResultRecorded(MatchResultRecorded {
            standing_id: cmd.standing_id.clone(),
            player_id: cmd.player_id.clone(),
            match_id: cmd.match_id,
            outcome: cmd.outcome,
        });
        let recalculated = Event::RatingRecalculated(RatingRecalculated {
            standing_id: cmd.standing_id,
            player_id: cmd.player_id,
            rating,
            rating_deviation,
            volatility,
            rated_matches: self.rated_matches,
        });
        self.root.record(Box::new(recorded.clone()));
        self.root.record(Box::new(recalculated.clone()));
        Ok(vec![recorded, recalculated])
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
            AWARD_STAR => {
                let cmd: AwardStar = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed AwardStarCmd payload: {e}"))
                })?;
                self.award_star(cmd)
            }
            ELEVATE_SMURF => {
                let cmd: ElevateSmurf = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!(
                        "malformed ElevateSmurfCmd payload: {e}"
                    ))
                })?;
                self.elevate_smurf(cmd)
            }
            RECORD_MATCH_RESULT => {
                let cmd: RecordMatchResult =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed RecordMatchResultCmd payload: {e}"
                        ))
                    })?;
                self.record_match_result(cmd)
            }
            APPLY_DISCONNECT_PENALTY => {
                let cmd: ApplyDisconnectPenalty = serde_json::from_slice(&command.payload)
                    .map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ApplyDisconnectPenaltyCmd payload: {e}"
                        ))
                    })?;
                self.apply_disconnect_penalty(cmd)
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
            other => panic!("expected RankFloorProtected, got {other:?}"),
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

    // --- ElevateSmurfCmd -------------------------------------------------

    /// An elevation-ready standing `r-01` for player `p-1`: a suspected smurf
    /// that has reached the 20-match threshold and has *not* yet been elevated,
    /// with a fresh Glicko-2 estimate, a well-formed visible rank at its Corner
    /// floor, and a disconnect ledger on the doubling schedule. Tests mutate one
    /// aspect at a time to drive a specific rejection.
    fn ready_smurf() -> RankedStanding {
        let mut standing = RankedStanding::new("r-01");
        standing.set_player("p-1");
        // Glicko-2 estimate recalculated after all 20 rated matches.
        standing.set_ratings(1720.0, 70.0, 0.058, 20, 20);
        // Two stars in Corner, no bonus star, no live streak.
        standing.set_visible_rank(Tier::Corner, 2, false, 0);
        // Reached floor is Corner (an anti-tilt bracket); current tier is at it.
        standing.set_tier_floor(Tier::Corner);
        // A suspected smurf, 20 matches played, not yet elevated.
        standing.set_smurf_state(SMURF_ELEVATION_MATCHES, true, false);
        // Two abandonments → BASE·2 = 10 under the doubling schedule.
        standing.set_disconnect_ledger(2, BASE_DISCONNECT_PENALTY * 2);
        standing
    }

    /// A command elevating `r-01` for player `p-1` after 20 matches.
    fn valid_elevate_cmd() -> ElevateSmurf {
        ElevateSmurf::new("r-01", "p-1", SMURF_ELEVATION_MATCHES)
    }

    // Scenario: Successfully execute ElevateSmurfCmd.
    #[test]
    fn elevates_smurf_and_emits_smurf_elevated_event() {
        let mut standing = ready_smurf();

        let events = standing
            .execute(valid_elevate_cmd().into_command())
            .expect("a warranted elevation should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "smurf.elevated");
        match &events[0] {
            Event::SmurfElevated(elevated) => {
                assert_eq!(elevated.standing_id, "r-01");
                assert_eq!(elevated.player_id, "p-1");
                assert_eq!(elevated.match_count, SMURF_ELEVATION_MATCHES);
                // Elevated one bracket up, from Corner to Contender.
                assert_eq!(elevated.elevated_to, Tier::Contender);
            }
            other => panic!("expected SmurfElevated, got {other:?}"),
        }
        // The standing was promoted and recorded the event.
        assert_eq!(standing.tier(), Tier::Contender);
        assert_eq!(standing.version(), 1);
        assert_eq!(standing.uncommitted_events().len(), 1);
        assert_eq!(
            standing.uncommitted_events()[0].event_type(),
            "smurf.elevated"
        );
    }

    // Scenario: rejected — Glicko-2 rating, RD, and volatility are recalculated
    // after every rated match.
    #[test]
    fn elevate_rejects_when_ratings_are_stale() {
        let mut standing = ready_smurf();
        // The estimate lags: 20 rated matches played, but synced at match 19.
        standing.set_ratings(1720.0, 70.0, 0.058, 20, 19);

        let err = standing
            .execute(valid_elevate_cmd().into_command())
            .expect_err("a stale Glicko-2 estimate must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — visible rank advances through tiers Block→Legend with
    // 3 stars per tier; a win streak grants a bonus star.
    #[test]
    fn elevate_rejects_when_visible_rank_exceeds_stars_per_tier() {
        let mut standing = ready_smurf();
        // Four stars in a tier that promotes at three is malformed.
        standing.set_visible_rank(Tier::Corner, STARS_PER_TIER + 1, false, 0);

        let err = standing
            .execute(valid_elevate_cmd().into_command())
            .expect_err("a tier over its star cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — rank-floor protection prevents demotion below a
    // reached tier floor (anti-tilt applies to Block/Corner).
    #[test]
    fn elevate_rejects_when_standing_is_below_its_floor() {
        let mut standing = ready_smurf();
        // Current tier Block sits below the reached Corner floor.
        standing.set_visible_rank(Tier::Block, 2, false, 0);

        let err = standing
            .execute(valid_elevate_cmd().into_command())
            .expect_err("a standing below its floor must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — a suspected smurf is auto-elevated to a higher bracket
    // after 20 matches. Here the standing is not an eligible smurf, so elevation
    // is unwarranted.
    #[test]
    fn elevate_rejects_when_not_an_eligible_smurf() {
        let mut standing = ready_smurf();
        // Not flagged as a suspected smurf: elevation is unwarranted.
        standing.set_smurf_state(SMURF_ELEVATION_MATCHES, false, false);

        let err = standing
            .execute(valid_elevate_cmd().into_command())
            .expect_err("elevating a non-smurf must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Smurf-elevation rule: a standing already elevated is not elevated again.
    #[test]
    fn elevate_rejects_when_already_elevated() {
        let mut standing = ready_smurf();
        // Already elevated: a smurf is elevated at most once.
        standing.set_smurf_state(SMURF_ELEVATION_MATCHES, true, true);

        let err = standing
            .execute(valid_elevate_cmd().into_command())
            .expect_err("re-elevating an elevated smurf must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — disconnect penalties escalate (doubling) on repeated
    // abandonment.
    #[test]
    fn elevate_rejects_when_disconnect_penalty_does_not_double() {
        let mut standing = ready_smurf();
        // Three abandonments owe BASE·4 under doubling; charging BASE·3 breaks it.
        standing.set_disconnect_ledger(3, BASE_DISCONNECT_PENALTY * 3);

        let err = standing
            .execute(valid_elevate_cmd().into_command())
            .expect_err("a penalty off the doubling schedule must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A valid matchCount must be supplied: it must agree with the standing's record.
    #[test]
    fn elevate_rejects_when_match_count_disagrees_with_record() {
        let mut standing = ready_smurf();
        // The standing has recorded 20 matches; a command reporting 19 is invalid.
        let cmd = ElevateSmurf::new("r-01", "p-1", SMURF_ELEVATION_MATCHES - 1);

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a match count that disagrees with the record must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A command naming a different standing is rejected before any invariant runs.
    #[test]
    fn elevate_rejects_command_for_a_different_standing() {
        let mut standing = ready_smurf();
        let cmd = ElevateSmurf::new("r-99", "p-1", SMURF_ELEVATION_MATCHES);

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a command for another standing must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A command with no player is rejected.
    #[test]
    fn elevate_rejects_command_without_a_player() {
        let mut standing = ready_smurf();
        let cmd = ElevateSmurf::new("r-01", "   ", SMURF_ELEVATION_MATCHES);

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a missing playerId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    #[test]
    fn elevate_command_payload_round_trips() {
        let cmd = valid_elevate_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, ElevateSmurf::COMMAND);
        let decoded: ElevateSmurf = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_elevate_cmd());
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

    // --- ApplyDisconnectPenaltyCmd ---------------------------------------

    /// A penalty-ready standing `r-01` for player `p-1`: a fresh Glicko-2
    /// estimate, a well-formed visible rank at its Corner floor, no pending smurf
    /// elevation, and a disconnect ledger on the doubling schedule (two prior
    /// abandonments owing BASE·2). Tests mutate one aspect at a time to drive a
    /// specific rejection.
    fn ready_disconnect() -> RankedStanding {
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

    /// A command charging a disconnect penalty to `r-01` for player `p-1` after
    /// the two prior abandonments the ready standing records.
    fn valid_disconnect_cmd() -> ApplyDisconnectPenalty {
        ApplyDisconnectPenalty::new("r-01", "p-1", 2)
    }

    // Scenario: Successfully execute ApplyDisconnectPenaltyCmd.
    #[test]
    fn applies_penalty_and_emits_disconnect_penalty_applied_event() {
        let mut standing = ready_disconnect();

        let events = standing
            .execute(valid_disconnect_cmd().into_command())
            .expect("a valid disconnect penalty should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "disconnect.penalty.applied");
        match &events[0] {
            Event::DisconnectPenaltyApplied(applied) => {
                assert_eq!(applied.standing_id, "r-01");
                assert_eq!(applied.player_id, "p-1");
                assert_eq!(applied.prior_penalties, 2);
                // The third abandonment doubles BASE·2 to BASE·4.
                assert_eq!(applied.penalty, BASE_DISCONNECT_PENALTY * 4);
                assert_eq!(applied.abandonments, 3);
            }
            other => panic!("expected DisconnectPenaltyApplied, got {other:?}"),
        }
        // The ledger advanced one step and stays on the doubling schedule.
        assert_eq!(standing.version(), 1);
        assert_eq!(standing.uncommitted_events().len(), 1);
        assert_eq!(
            standing.uncommitted_events()[0].event_type(),
            "disconnect.penalty.applied"
        );
    }

    // Scenario: rejected — Glicko-2 rating, RD, and volatility are recalculated
    // after every rated match.
    #[test]
    fn disconnect_rejects_when_ratings_are_stale() {
        let mut standing = ready_disconnect();
        // The estimate lags: 10 rated matches played, but synced at match 9.
        standing.set_ratings(1620.0, 80.0, 0.059, 10, 9);

        let err = standing
            .execute(valid_disconnect_cmd().into_command())
            .expect_err("a stale Glicko-2 estimate must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — visible rank advances through tiers Block→Legend with
    // 3 stars per tier; a win streak grants a bonus star.
    #[test]
    fn disconnect_rejects_when_visible_rank_exceeds_stars_per_tier() {
        let mut standing = ready_disconnect();
        // Four stars in a tier that promotes at three is malformed.
        standing.set_visible_rank(Tier::Corner, STARS_PER_TIER + 1, false, 0);

        let err = standing
            .execute(valid_disconnect_cmd().into_command())
            .expect_err("a tier over its star cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — rank-floor protection prevents demotion below a
    // reached tier floor (anti-tilt applies to Block/Corner).
    #[test]
    fn disconnect_rejects_when_standing_is_below_its_floor() {
        let mut standing = ready_disconnect();
        // Current tier Block sits below the reached Corner floor.
        standing.set_visible_rank(Tier::Block, 2, false, 0);

        let err = standing
            .execute(valid_disconnect_cmd().into_command())
            .expect_err("a standing below its floor must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — a suspected smurf is auto-elevated to a higher bracket
    // after 20 matches.
    #[test]
    fn disconnect_rejects_suspected_smurf_not_yet_elevated() {
        let mut standing = ready_disconnect();
        // A suspected smurf past the 20-match threshold that was not elevated.
        standing.set_smurf_state(SMURF_ELEVATION_MATCHES, true, false);

        let err = standing
            .execute(valid_disconnect_cmd().into_command())
            .expect_err("an un-elevated suspected smurf must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — disconnect penalties escalate (doubling) on repeated
    // abandonment. Here the *existing* ledger is off the schedule, so the penalty
    // cannot be advanced from an inconsistent base.
    #[test]
    fn disconnect_rejects_when_existing_penalty_does_not_double() {
        let mut standing = ready_disconnect();
        // Three abandonments owe BASE·4 under doubling; charging BASE·3 breaks it.
        standing.set_disconnect_ledger(3, BASE_DISCONNECT_PENALTY * 3);

        // The command must still reconcile with the (broken) recorded count.
        let cmd = ApplyDisconnectPenalty::new("r-01", "p-1", 3);
        let err = standing
            .execute(cmd.into_command())
            .expect_err("a penalty off the doubling schedule must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A valid priorPenalties must be supplied: it must agree with the standing's
    // recorded abandonment count.
    #[test]
    fn disconnect_rejects_when_prior_penalties_disagree_with_record() {
        let mut standing = ready_disconnect();
        // The standing has recorded two abandonments; a command reporting one is
        // invalid — it would restart the escalation from the wrong place.
        let cmd = ApplyDisconnectPenalty::new("r-01", "p-1", 1);

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a prior-penalty count that disagrees with the record must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A command naming a different standing is rejected before any invariant runs.
    #[test]
    fn disconnect_rejects_command_for_a_different_standing() {
        let mut standing = ready_disconnect();
        let cmd = ApplyDisconnectPenalty::new("r-99", "p-1", 2);

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a command for another standing must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A command with no player is rejected.
    #[test]
    fn disconnect_rejects_command_without_a_player() {
        let mut standing = ready_disconnect();
        let cmd = ApplyDisconnectPenalty::new("r-01", "   ", 2);

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a missing playerId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // The very first abandonment charges the BASE penalty from an empty ledger.
    #[test]
    fn disconnect_charges_base_penalty_on_first_abandonment() {
        let mut standing = ready_disconnect();
        // No prior abandonments: an empty, consistent ledger.
        standing.set_disconnect_ledger(0, 0);

        let events = standing
            .execute(ApplyDisconnectPenalty::new("r-01", "p-1", 0).into_command())
            .expect("a first disconnect penalty should succeed");
        match &events[0] {
            Event::DisconnectPenaltyApplied(applied) => {
                assert_eq!(applied.prior_penalties, 0);
                assert_eq!(applied.penalty, BASE_DISCONNECT_PENALTY);
                assert_eq!(applied.abandonments, 1);
            }
            other => panic!("expected DisconnectPenaltyApplied, got {other:?}"),
        }
        assert_eq!(standing.version(), 1);
    }

    #[test]
    fn disconnect_command_payload_round_trips() {
        let cmd = valid_disconnect_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, ApplyDisconnectPenalty::COMMAND);
        let decoded: ApplyDisconnectPenalty = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_disconnect_cmd());
    }

    // ----- RecordMatchResultCmd (S-27) -----------------------------------

    /// A result-ready standing: same consistent state as [`ready_standing`],
    /// reused because `RecordMatchResultCmd` enforces the identical five
    /// invariants as preconditions before ingesting a match.
    fn result_ready_standing() -> RankedStanding {
        ready_standing()
    }

    /// A command recording a win against a 1600-rated opponent on `r-01` for
    /// player `p-1`.
    fn valid_result_cmd() -> RecordMatchResult {
        RecordMatchResult::new("r-01", "p-1", "m-1", MatchOutcome::Win, 1600.0)
    }

    // Scenario: Successfully execute RecordMatchResultCmd — a match.result.recorded
    // event and a rating.recalculated event are emitted.
    #[test]
    fn records_result_and_emits_recorded_and_recalculated_events() {
        let mut standing = result_ready_standing();
        let before_matches = standing.rated_matches;

        let events = standing
            .execute(valid_result_cmd().into_command())
            .expect("valid match result should succeed");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type(), "match.result.recorded");
        assert_eq!(events[1].event_type(), "rating.recalculated");
        match &events[0] {
            Event::MatchResultRecorded(recorded) => {
                assert_eq!(recorded.standing_id, "r-01");
                assert_eq!(recorded.player_id, "p-1");
                assert_eq!(recorded.match_id, "m-1");
                assert_eq!(recorded.outcome, MatchOutcome::Win);
            }
            other => panic!("expected MatchResultRecorded, got {other:?}"),
        }
        match &events[1] {
            Event::RatingRecalculated(recalc) => {
                assert_eq!(recalc.standing_id, "r-01");
                assert_eq!(recalc.rated_matches, before_matches + 1);
                // A win raises the rating and shrinks the deviation.
                assert!(recalc.rating > 1620.0);
                assert!(recalc.rating_deviation < 80.0);
                assert!(recalc.volatility > 0.0);
            }
            other => panic!("expected RatingRecalculated, got {other:?}"),
        }
        // Both events were recorded and the estimate stays fresh for the next
        // match (ratings_synced_at now equals the new rated_matches).
        assert_eq!(standing.rated_matches, before_matches + 1);
        assert_eq!(standing.version(), 2);
        assert_eq!(standing.uncommitted_events().len(), 2);
        standing
            .ensure_ratings_recalculated()
            .expect("estimate must be fresh after recording");
    }

    // A loss lowers the rating.
    #[test]
    fn a_loss_lowers_the_rating() {
        let mut standing = result_ready_standing();
        let cmd = RecordMatchResult::new("r-01", "p-1", "m-2", MatchOutcome::Loss, 1600.0);

        standing
            .execute(cmd.into_command())
            .expect("a recorded loss should succeed");

        assert!(standing.rating() < 1620.0);
    }

    // Scenario: rejected — Glicko-2 rating, RD, and volatility are recalculated
    // after every rated match.
    #[test]
    fn record_rejects_when_ratings_are_stale() {
        let mut standing = result_ready_standing();
        // The estimate lags the matches already played.
        standing.set_ratings(1620.0, 80.0, 0.059, 10, 9);

        let err = standing
            .execute(valid_result_cmd().into_command())
            .expect_err("a stale Glicko-2 estimate must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — visible rank advances through tiers Block→Legend with
    // 3 stars per tier; a win streak grants a bonus star.
    #[test]
    fn record_rejects_malformed_visible_rank() {
        let mut standing = result_ready_standing();
        standing.set_visible_rank(Tier::Corner, STARS_PER_TIER + 1, false, 0);

        let err = standing
            .execute(valid_result_cmd().into_command())
            .expect_err("a tier over its star cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — rank-floor protection prevents demotion below a reached
    // tier floor (anti-tilt applies to Block/Corner).
    #[test]
    fn record_rejects_when_below_floor() {
        let mut standing = result_ready_standing();
        // Current tier Block sits below the reached Corner floor.
        standing.set_visible_rank(Tier::Block, 2, false, 0);

        let err = standing
            .execute(valid_result_cmd().into_command())
            .expect_err("a standing below its floor must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — a suspected smurf is auto-elevated to a higher bracket
    // after 20 matches.
    #[test]
    fn record_rejects_unelevated_smurf() {
        let mut standing = result_ready_standing();
        standing.set_smurf_state(SMURF_ELEVATION_MATCHES, true, false);

        let err = standing
            .execute(valid_result_cmd().into_command())
            .expect_err("an un-elevated suspected smurf must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — disconnect penalties escalate (doubling) on repeated
    // abandonment.
    #[test]
    fn record_rejects_penalty_off_doubling_schedule() {
        let mut standing = result_ready_standing();
        // Three abandonments owe BASE·4 under doubling; charging BASE·3 breaks it.
        standing.set_disconnect_ledger(3, BASE_DISCONNECT_PENALTY * 3);

        let err = standing
            .execute(valid_result_cmd().into_command())
            .expect_err("a penalty off the doubling schedule must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A result for the wrong standing / player / with a missing match or a
    // non-finite opponent rating is rejected before any state changes.
    #[test]
    fn record_rejects_bad_targets_and_inputs() {
        for cmd in [
            RecordMatchResult::new("r-99", "p-1", "m-1", MatchOutcome::Win, 1600.0),
            RecordMatchResult::new("r-01", "p-other", "m-1", MatchOutcome::Win, 1600.0),
            RecordMatchResult::new("r-01", "   ", "m-1", MatchOutcome::Win, 1600.0),
            RecordMatchResult::new("r-01", "p-1", "  ", MatchOutcome::Win, 1600.0),
            RecordMatchResult::new("r-01", "p-1", "m-1", MatchOutcome::Win, 0.0),
            RecordMatchResult::new("r-01", "p-1", "m-1", MatchOutcome::Win, f64::NAN),
        ] {
            let mut standing = result_ready_standing();
            let err = standing
                .execute(cmd.into_command())
                .expect_err("a malformed target/input must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(standing.version(), 0);
        }
    }

    #[test]
    fn record_command_payload_round_trips() {
        let cmd = valid_result_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, RecordMatchResult::COMMAND);
        let decoded: RecordMatchResult = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_result_cmd());
    }

    // --- AwardStarCmd ----------------------------------------------------

    /// An award-ready standing `r-01` for player `p-1`, one star short of the tier
    /// cap so a single awarded star promotes it: fresh Glicko-2 estimate, two
    /// stars in Corner sitting at its reached Corner floor, no pending smurf
    /// elevation, and a disconnect ledger on the doubling schedule.
    fn award_ready_standing() -> RankedStanding {
        let mut standing = RankedStanding::new("r-01");
        standing.set_player("p-1");
        standing.set_ratings(1620.0, 80.0, 0.059, 10, 10);
        // Two stars in Corner (cap is 3): the next star promotes to Contender.
        standing.set_visible_rank(Tier::Corner, STARS_PER_TIER - 1, false, 0);
        standing.set_tier_floor(Tier::Corner);
        standing.set_smurf_state(10, false, false);
        standing.set_disconnect_ledger(2, BASE_DISCONNECT_PENALTY * 2);
        standing
    }

    /// A command awarding a star (no streak bonus) to `r-01` for player `p-1`.
    fn award_cmd() -> AwardStar {
        AwardStar::new("r-01", "p-1", false)
    }

    // Scenario: Successfully execute AwardStarCmd — a star.awarded event and a
    // rank.promoted event are both emitted.
    #[test]
    fn awards_star_and_promotes_rank_emitting_both_events() {
        let mut standing = award_ready_standing();

        let events = standing
            .execute(award_cmd().into_command())
            .expect("awarding a tier-filling star should succeed");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type(), "star.awarded");
        assert_eq!(events[1].event_type(), "rank.promoted");
        match &events[0] {
            Event::StarAwarded(awarded) => {
                assert_eq!(awarded.standing_id, "r-01");
                assert_eq!(awarded.player_id, "p-1");
                assert_eq!(awarded.tier, Tier::Corner);
                assert_eq!(awarded.stars_awarded, 1);
                assert!(!awarded.streak_bonus);
            }
            other => panic!("expected StarAwarded, got {other:?}"),
        }
        match &events[1] {
            Event::RankPromoted(promoted) => {
                assert_eq!(promoted.standing_id, "r-01");
                assert_eq!(promoted.player_id, "p-1");
                assert_eq!(promoted.from_tier, Tier::Corner);
                assert_eq!(promoted.to_tier, Tier::Contender);
            }
            other => panic!("expected RankPromoted, got {other:?}"),
        }
        // Promoted into Contender with the surplus (0) stars; two events recorded.
        assert_eq!(standing.tier(), Tier::Contender);
        assert_eq!(standing.version(), 2);
        assert_eq!(standing.uncommitted_events().len(), 2);
    }

    // A win-streak win grants a bonus star on top of the base star.
    #[test]
    fn awards_bonus_star_on_a_live_win_streak() {
        let mut standing = RankedStanding::new("r-01");
        standing.set_player("p-1");
        standing.set_ratings(1620.0, 80.0, 0.059, 10, 10);
        // One star in Corner with a live streak: base + bonus = 2 -> fills the cap.
        standing.set_visible_rank(Tier::Corner, STARS_PER_TIER - 2, false, 3);
        standing.set_tier_floor(Tier::Corner);
        standing.set_smurf_state(10, false, false);
        standing.set_disconnect_ledger(2, BASE_DISCONNECT_PENALTY * 2);

        let events = standing
            .execute(AwardStar::new("r-01", "p-1", true).into_command())
            .expect("a streak win should award a bonus star");

        assert_eq!(events.len(), 2);
        match &events[0] {
            Event::StarAwarded(awarded) => {
                assert_eq!(awarded.stars_awarded, 2);
                assert!(awarded.streak_bonus);
            }
            other => panic!("expected StarAwarded, got {other:?}"),
        }
        assert_eq!(standing.tier(), Tier::Contender);
    }

    // Scenario: rejected — Glicko-2 rating, RD, and volatility are recalculated
    // after every rated match.
    #[test]
    fn award_rejects_when_ratings_are_stale() {
        let mut standing = award_ready_standing();
        standing.set_ratings(1620.0, 80.0, 0.059, 10, 9);

        let err = standing
            .execute(award_cmd().into_command())
            .expect_err("a stale Glicko-2 estimate must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — visible rank advances through tiers Block→Legend with
    // 3 stars per tier; a win streak grants a bonus star.
    #[test]
    fn award_rejects_when_visible_rank_exceeds_stars_per_tier() {
        let mut standing = award_ready_standing();
        standing.set_visible_rank(Tier::Corner, STARS_PER_TIER + 1, false, 0);

        let err = standing
            .execute(award_cmd().into_command())
            .expect_err("a tier over its star cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Visible-rank invariant: a streak bonus star cannot be claimed without a live
    // win streak.
    #[test]
    fn award_rejects_streak_bonus_without_a_win_streak() {
        let mut standing = award_ready_standing();

        let err = standing
            .execute(AwardStar::new("r-01", "p-1", true).into_command())
            .expect_err("a streak bonus without a streak must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — rank-floor protection prevents demotion below a
    // reached tier floor (anti-tilt applies to Block/Corner).
    #[test]
    fn award_rejects_when_standing_is_below_its_floor() {
        let mut standing = award_ready_standing();
        // Current tier Block sits below the reached Corner floor.
        standing.set_visible_rank(Tier::Block, 2, false, 0);

        let err = standing
            .execute(award_cmd().into_command())
            .expect_err("a standing below its floor must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — a suspected smurf is auto-elevated to a higher bracket
    // after 20 matches.
    #[test]
    fn award_rejects_suspected_smurf_not_yet_elevated() {
        let mut standing = award_ready_standing();
        standing.set_smurf_state(SMURF_ELEVATION_MATCHES, true, false);

        let err = standing
            .execute(award_cmd().into_command())
            .expect_err("an un-elevated suspected smurf must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — disconnect penalties escalate (doubling) on repeated
    // abandonment.
    #[test]
    fn award_rejects_when_disconnect_penalty_does_not_double() {
        let mut standing = award_ready_standing();
        standing.set_disconnect_ledger(3, BASE_DISCONNECT_PENALTY * 3);

        let err = standing
            .execute(award_cmd().into_command())
            .expect_err("a penalty off the doubling schedule must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A command naming a different standing or player is rejected before crediting.
    #[test]
    fn award_rejects_command_for_a_different_standing_or_player() {
        let mut standing = award_ready_standing();
        let wrong_standing = standing
            .execute(AwardStar::new("r-99", "p-1", false).into_command())
            .expect_err("a command for another standing must be rejected");
        assert!(matches!(wrong_standing, DomainError::InvariantViolation(_)));

        let wrong_player = standing
            .execute(AwardStar::new("r-01", "p-other", false).into_command())
            .expect_err("a command for another player must be rejected");
        assert!(matches!(wrong_player, DomainError::InvariantViolation(_)));

        let no_player = standing
            .execute(AwardStar::new("r-01", "   ", false).into_command())
            .expect_err("a missing playerId must be rejected");
        assert!(matches!(no_player, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A mid-tier win that does not fill the cap awards a star without promoting.
    #[test]
    fn award_without_filling_tier_emits_only_star_awarded() {
        let mut standing = award_ready_standing();
        // One star in Corner: awarding one more reaches two, below the cap of 3.
        standing.set_visible_rank(Tier::Corner, STARS_PER_TIER - 2, false, 0);

        let events = standing
            .execute(award_cmd().into_command())
            .expect("awarding a non-filling star should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "star.awarded");
        assert_eq!(standing.tier(), Tier::Corner);
        assert_eq!(standing.version(), 1);
    }

    #[test]
    fn award_command_payload_round_trips() {
        let cmd = AwardStar::new("r-01", "p-1", true);
        let command = cmd.into_command();
        assert_eq!(command.name, AwardStar::COMMAND);
        let decoded: AwardStar = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, cmd);
    }
}
