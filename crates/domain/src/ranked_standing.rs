//! RankedStanding bounded context — a player's competitive rank/rating over a season.
//!
//! A [`RankedStanding`] is one player's ranked record for a season: the hidden
//! Glicko-2 skill estimate (rating, rating deviation, volatility) plus the
//! *visible* ladder position (tier and stars) derived from it. Ingesting a
//! completed ranked result recomputes the rating and re-derives the ladder.
//! Five invariants govern a standing:
//!
//! 1. **Glicko-2 consistency** — the rating, RD, and volatility are recalculated
//!    after *every* rated match, so the recalculation count never lags the rated
//!    match count and RD/volatility stay strictly positive.
//! 2. **Visible ladder structure** — the ladder runs Block → Corner → Contender
//!    → Champion → Legend with [`STARS_PER_TIER`] (3) stars per tier; a win
//!    streak may grant a bonus star, but the star count never exceeds the
//!    per-tier cap.
//! 3. **Rank-floor protection** — once a tier is reached its floor holds; a
//!    standing may never sit below its reached floor (anti-tilt, most relevant to
//!    the Block/Corner tiers).
//! 4. **Smurf elevation** — a suspected smurf is auto-elevated to a higher
//!    bracket after [`SMURF_ELEVATION_MATCHES`] (20) matches; leaving one
//!    un-elevated past that threshold is a violation.
//! 5. **Disconnect escalation** — abandonment penalties double on every repeat,
//!    so the recorded penalty must equal the doubling schedule for the number of
//!    disconnects.
//!
//! One command is implemented. [`RecordMatchResult`] (`RecordMatchResultCmd`)
//! ingests a completed ranked result for this standing, enforces every invariant,
//! recomputes the Glicko-2 rating, and on success emits **two** events:
//! [`Event::MatchResultRecorded`] (`match.result.recorded`) followed by
//! [`Event::RatingRecalculated`] (`rating.recalculated`).
//!
//! This module is hand-written (it no longer uses `shared::stub_aggregate!`) but
//! preserves the same public surface — a [`RankedStanding`] aggregate and a
//! [`RankedStandingRepository`] port — so the persistence adapters in
//! `crates/mocks` keep compiling unchanged.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "RankedStanding";

/// The command name [`RankedStanding::execute`] recognizes for ingesting a
/// completed ranked result and recomputing the rating.
const RECORD_MATCH_RESULT: &str = "RecordMatchResultCmd";

/// Stars per visible tier: a standing advances through 3 stars, then promotes to
/// the next tier. A win streak may grant a bonus star but never pushes the count
/// past this cap.
pub const STARS_PER_TIER: u8 = 3;

/// A suspected smurf is auto-elevated to a higher bracket after this many
/// matches; leaving one un-elevated past the threshold violates invariant 4.
pub const SMURF_ELEVATION_MATCHES: u32 = 20;

/// The base abandonment penalty (in ranked points) for a first disconnect;
/// each repeat doubles it (`BASE_DISCONNECT_PENALTY * 2^(n-1)` for the n-th).
pub const BASE_DISCONNECT_PENALTY: u32 = 5;

/// Floor for Glicko-2 volatility — it decays toward stability as a standing
/// accumulates games but never collapses to zero.
pub const MIN_VOLATILITY: f64 = 0.01;

/// The visible ranked ladder, ascending Block → Legend. Discriminants are the
/// tier ordinals used by rank-floor comparisons; a higher ordinal is a higher
/// tier. Anti-tilt (rank-floor protection) is most relevant to the two lowest
/// tiers, [`Tier::Block`] and [`Tier::Corner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Tier {
    /// Entry tier.
    Block = 0,
    /// Second tier.
    Corner = 1,
    /// Third tier.
    Contender = 2,
    /// Fourth tier.
    Champion = 3,
    /// Apex tier.
    Legend = 4,
}

/// The result of a rated match from this standing's perspective, carried by the
/// `RecordMatchResultCmd` payload. The Glicko-2 update scores a win as `1.0`, a
/// draw as `0.5`, and a loss as `0.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchOutcome {
    /// This player won the rated match.
    Win,
    /// The rated match was a draw.
    Draw,
    /// This player lost the rated match.
    Loss,
}

impl MatchOutcome {
    /// Glicko-2 score for this outcome: 1.0 win, 0.5 draw, 0.0 loss.
    fn score(self) -> f64 {
        match self {
            MatchOutcome::Win => 1.0,
            MatchOutcome::Draw => 0.5,
            MatchOutcome::Loss => 0.0,
        }
    }
}

/// The `RecordMatchResultCmd` payload: a completed rated result for `player_id`
/// against an opponent rated `opponent_rating`. Field names are the ranked
/// service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`RecordMatchResult::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`RankedStanding::execute`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordMatchResult {
    /// Identity of the player whose standing this result updates; must name the
    /// player this aggregate records.
    pub player_id: String,
    /// Identity of the completed match producing this result. Must be non-empty.
    pub match_id: String,
    /// The rated outcome from `player_id`'s perspective.
    pub outcome: MatchOutcome,
    /// The opponent's Glicko-2 rating going into the match. Must be positive.
    pub opponent_rating: f64,
}

impl RecordMatchResult {
    /// The command name this maps to.
    pub const COMMAND: &'static str = RECORD_MATCH_RESULT;

    /// Build a command recording `outcome` for `player_id` in `match_id` against
    /// an opponent rated `opponent_rating`.
    pub fn new(
        player_id: impl Into<String>,
        match_id: impl Into<String>,
        outcome: MatchOutcome,
        opponent_rating: f64,
    ) -> Self {
        Self {
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

/// The recorded ranked result, carried by [`Event::MatchResultRecorded`] and thus
/// by the emitted `match.result.recorded` event.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchResultRecorded {
    /// The player whose standing was updated.
    pub player_id: String,
    /// The match that produced the result.
    pub match_id: String,
    /// The rated outcome from the player's perspective.
    pub outcome: MatchOutcome,
    /// The opponent's rating going into the match.
    pub opponent_rating: f64,
}

/// The recomputed Glicko-2 skill estimate, carried by
/// [`Event::RatingRecalculated`] and thus by the emitted `rating.recalculated`
/// event.
#[derive(Debug, Clone, PartialEq)]
pub struct RatingRecalculated {
    /// The player whose rating was recalculated.
    pub player_id: String,
    /// The new Glicko-2 rating.
    pub rating: f64,
    /// The new rating deviation (RD).
    pub rating_deviation: f64,
    /// The new volatility.
    pub volatility: f64,
}

/// Domain events emitted by [`RankedStanding`]. A successful
/// `RecordMatchResultCmd` emits both, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A completed rated result was ingested for the standing.
    MatchResultRecorded(MatchResultRecorded),
    /// The Glicko-2 rating, RD, and volatility were recalculated afterward.
    RatingRecalculated(RatingRecalculated),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::MatchResultRecorded(_) => "match.result.recorded",
            Event::RatingRecalculated(_) => "rating.recalculated",
        }
    }
}

/// The RankedStanding aggregate: one player's ranked record for a season.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring — the in-memory
/// repository adapters, the server — is unchanged, while it now carries the
/// standing's ranked state: the hidden Glicko-2 estimate, how many rated matches
/// have been ingested and recalculated, the visible tier/stars ladder position
/// and its reached floor, smurf-elevation bookkeeping, and the disconnect
/// escalation counters. Its `execute` handles [`RecordMatchResultCmd`].
///
/// A fresh standing from [`RankedStanding::new`] is a consistent, freshly-seeded
/// record (unplayed, at the Block floor, no disconnects); the configuration
/// methods below drive it to the state a command validates, exactly as
/// [`MatchmakingTicket`](crate::matchmaking_ticket) is built up before a command
/// validates it.
#[derive(Debug)]
pub struct RankedStanding {
    id: String,
    root: AggregateRoot,
    /// The player this standing belongs to. A command must name this player.
    player_id: String,
    /// Hidden Glicko-2 rating.
    rating: f64,
    /// Hidden Glicko-2 rating deviation (RD); must stay strictly positive.
    rating_deviation: f64,
    /// Hidden Glicko-2 volatility; must stay strictly positive.
    volatility: f64,
    /// Rated matches ingested so far.
    matches_played: u32,
    /// Times the rating has been recalculated. Invariant 1 requires this to keep
    /// pace with `matches_played` — the rating is recomputed after every match.
    recalculations: u32,
    /// Current visible tier.
    tier: Tier,
    /// Stars accrued within `tier`; capped by [`STARS_PER_TIER`].
    stars: u8,
    /// Current win streak; a streak may grant a bonus star.
    win_streak: u32,
    /// The lowest tier the standing may fall to — the highest tier ever reached.
    floor_tier: Tier,
    /// Whether this standing is flagged as a suspected smurf.
    suspected_smurf: bool,
    /// Whether a flagged smurf has been elevated to a higher bracket.
    elevated: bool,
    /// Number of ranked-match abandonments recorded.
    disconnect_count: u32,
    /// The currently-applied abandonment penalty; must equal the doubling
    /// schedule for `disconnect_count`.
    disconnect_penalty: u32,
}

impl RankedStanding {
    /// Create a new, freshly-seeded standing identified by `id`: unplayed, at the
    /// default Glicko-2 seed (rating 1500, RD 350, volatility 0.06), sitting at
    /// the Block floor with no stars, no smurf flag, and no disconnects. Use the
    /// configuration methods to drive it to the state a command validates.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            player_id: String::new(),
            rating: 1500.0,
            rating_deviation: 350.0,
            volatility: 0.06,
            matches_played: 0,
            recalculations: 0,
            tier: Tier::Block,
            stars: 0,
            win_streak: 0,
            floor_tier: Tier::Block,
            suspected_smurf: false,
            elevated: false,
            disconnect_count: 0,
            disconnect_penalty: 0,
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The player this standing belongs to.
    pub fn player_id(&self) -> &str {
        &self.player_id
    }

    /// Current Glicko-2 rating.
    pub fn rating(&self) -> f64 {
        self.rating
    }

    /// Current rating deviation (RD).
    pub fn rating_deviation(&self) -> f64 {
        self.rating_deviation
    }

    /// Current volatility.
    pub fn volatility(&self) -> f64 {
        self.volatility
    }

    /// Current visible tier.
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// Set the player this standing belongs to.
    pub fn set_player(&mut self, player_id: impl Into<String>) {
        self.player_id = player_id.into();
    }

    /// Set the hidden Glicko-2 estimate (rating, RD, volatility).
    pub fn set_rating(&mut self, rating: f64, rating_deviation: f64, volatility: f64) {
        self.rating = rating;
        self.rating_deviation = rating_deviation;
        self.volatility = volatility;
    }

    /// Set the rated-match history: how many matches have been ingested and how
    /// many times the rating has been recalculated. Invariant 1 requires these to
    /// stay equal.
    pub fn set_match_history(&mut self, matches_played: u32, recalculations: u32) {
        self.matches_played = matches_played;
        self.recalculations = recalculations;
    }

    /// Set the visible ladder position: current tier, stars within it, and win
    /// streak.
    pub fn set_ladder(&mut self, tier: Tier, stars: u8, win_streak: u32) {
        self.tier = tier;
        self.stars = stars;
        self.win_streak = win_streak;
    }

    /// Set the reached rank floor: the standing may never fall below this tier.
    pub fn set_floor(&mut self, floor_tier: Tier) {
        self.floor_tier = floor_tier;
    }

    /// Set the smurf-elevation bookkeeping: whether the standing is flagged and
    /// whether it has been elevated.
    pub fn set_smurf(&mut self, suspected_smurf: bool, elevated: bool) {
        self.suspected_smurf = suspected_smurf;
        self.elevated = elevated;
    }

    /// Set the disconnect escalation state: how many abandonments have occurred
    /// and the currently-applied penalty.
    pub fn set_disconnects(&mut self, disconnect_count: u32, disconnect_penalty: u32) {
        self.disconnect_count = disconnect_count;
        self.disconnect_penalty = disconnect_penalty;
    }

    /// The abandonment penalty the doubling schedule prescribes for `count`
    /// disconnects: `0` for none, else `BASE_DISCONNECT_PENALTY * 2^(count-1)`.
    fn expected_disconnect_penalty(count: u32) -> u32 {
        if count == 0 {
            0
        } else {
            BASE_DISCONNECT_PENALTY.saturating_mul(1u32 << (count - 1).min(31))
        }
    }

    /// Invariant 1: the Glicko-2 rating, RD, and volatility are recalculated
    /// after every rated match — the recalculation count keeps pace with the
    /// rated-match count, and RD/volatility stay strictly positive.
    fn ensure_rating_recalculated_each_match(&self) -> Result<(), DomainError> {
        if self.recalculations != self.matches_played {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' has {} rated matches but only {} rating recalculations; the \
                 Glicko-2 rating must be recalculated after every rated match",
                self.id, self.matches_played, self.recalculations
            )));
        }
        if !(self.rating_deviation > 0.0) || !(self.volatility > 0.0) {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' has non-positive RD ({}) or volatility ({}); the Glicko-2 estimate \
                 must remain well-defined after every recalculation",
                self.id, self.rating_deviation, self.volatility
            )));
        }
        Ok(())
    }

    /// Invariant 2: the visible rank advances through tiers with
    /// [`STARS_PER_TIER`] stars per tier; a win streak may grant a bonus star but
    /// the star count never exceeds the per-tier cap.
    fn ensure_ladder_structure(&self) -> Result<(), DomainError> {
        if self.stars > STARS_PER_TIER {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' holds {} stars in tier {:?} but a tier caps at {} stars (a win \
                 streak may grant a bonus star, never breach the cap)",
                self.id, self.stars, self.tier, STARS_PER_TIER
            )));
        }
        Ok(())
    }

    /// Invariant 3: rank-floor protection — a standing may never sit below its
    /// reached tier floor (anti-tilt, most relevant to Block/Corner).
    fn ensure_above_tier_floor(&self) -> Result<(), DomainError> {
        if self.tier < self.floor_tier {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' is at tier {:?}, below its reached floor {:?}; rank-floor protection \
                 forbids demotion beneath a reached tier",
                self.id, self.tier, self.floor_tier
            )));
        }
        Ok(())
    }

    /// Invariant 4: a suspected smurf is auto-elevated to a higher bracket after
    /// [`SMURF_ELEVATION_MATCHES`] matches; one still un-elevated past the
    /// threshold is a violation.
    fn ensure_smurf_elevated(&self) -> Result<(), DomainError> {
        if self.suspected_smurf && self.matches_played >= SMURF_ELEVATION_MATCHES && !self.elevated
        {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' is a suspected smurf with {} matches (>= {}) but has not been \
                 elevated to a higher bracket",
                self.id, self.matches_played, SMURF_ELEVATION_MATCHES
            )));
        }
        Ok(())
    }

    /// Invariant 5: disconnect penalties escalate by doubling — the applied
    /// penalty must equal the doubling schedule for the recorded disconnect
    /// count.
    fn ensure_disconnect_penalty_escalates(&self) -> Result<(), DomainError> {
        let expected = Self::expected_disconnect_penalty(self.disconnect_count);
        if self.disconnect_penalty != expected {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' has {} disconnects with penalty {} but the doubling schedule \
                 requires {}; abandonment penalties escalate by doubling on every repeat",
                self.id, self.disconnect_count, self.disconnect_penalty, expected
            )));
        }
        Ok(())
    }

    /// A Glicko-style rating step: recompute rating and RD from the outcome and
    /// the opponent's rating, and let volatility decay toward [`MIN_VOLATILITY`]
    /// as the estimate stabilizes. Returns `(rating, rd, volatility)`.
    fn recompute_rating(&self, outcome: MatchOutcome, opponent_rating: f64) -> (f64, f64, f64) {
        // Glicko constants on the rating scale.
        let q = std::f64::consts::LN_10 / 400.0;
        let pi_sq = std::f64::consts::PI * std::f64::consts::PI;
        let rd = self.rating_deviation;

        // Attenuation of the opponent's influence by this standing's own RD.
        let g = 1.0 / (1.0 + 3.0 * q * q * rd * rd / pi_sq).sqrt();
        // Expected score against the opponent.
        let expected = 1.0 / (1.0 + 10f64.powf(-g * (self.rating - opponent_rating) / 400.0));

        // Estimated variance of the rating from this single game.
        let d_sq = 1.0 / (q * q * g * g * expected * (1.0 - expected)).max(f64::MIN_POSITIVE);
        let new_rd = (1.0 / (1.0 / (rd * rd) + 1.0 / d_sq)).sqrt();
        let new_rating = self.rating + q * new_rd * new_rd * g * (outcome.score() - expected);
        let new_volatility = (self.volatility * 0.99).max(MIN_VOLATILITY);

        (new_rating, new_rd, new_volatility)
    }

    /// Handle `RecordMatchResultCmd`: verify the command targets this standing and
    /// carries a well-formed result, enforce every invariant on the current state,
    /// recompute the Glicko-2 rating, and emit [`Event::MatchResultRecorded`]
    /// followed by [`Event::RatingRecalculated`].
    fn record_match_result(&mut self, cmd: RecordMatchResult) -> Result<Vec<Event>, DomainError> {
        // The command must name the player this aggregate actually records.
        if cmd.player_id != self.player_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets player '{}' but this standing records '{}'",
                cmd.player_id, self.player_id
            )));
        }
        // A completed result must name the match that produced it.
        if cmd.match_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' cannot record a result without a match id",
                self.id
            )));
        }
        // Glicko-2 needs a well-defined opponent rating to compute against.
        if !(cmd.opponent_rating > 0.0) {
            return Err(DomainError::InvariantViolation(format!(
                "standing '{}' received a non-positive opponent rating {}",
                self.id, cmd.opponent_rating
            )));
        }

        // Enforce every invariant on the pre-existing state before ingesting.
        self.ensure_rating_recalculated_each_match()?;
        self.ensure_ladder_structure()?;
        self.ensure_above_tier_floor()?;
        self.ensure_smurf_elevated()?;
        self.ensure_disconnect_penalty_escalates()?;

        // Recompute the hidden Glicko-2 estimate from the result.
        let (rating, rating_deviation, volatility) =
            self.recompute_rating(cmd.outcome, cmd.opponent_rating);

        let recorded = Event::MatchResultRecorded(MatchResultRecorded {
            player_id: cmd.player_id.clone(),
            match_id: cmd.match_id,
            outcome: cmd.outcome,
            opponent_rating: cmd.opponent_rating,
        });
        let recalculated = Event::RatingRecalculated(RatingRecalculated {
            player_id: cmd.player_id,
            rating,
            rating_deviation,
            volatility,
        });

        // Apply the result: ingest the match and keep the recalculation count in
        // lockstep, so invariant 1 continues to hold for the next command.
        self.rating = rating;
        self.rating_deviation = rating_deviation;
        self.volatility = volatility;
        self.matches_played += 1;
        self.recalculations += 1;

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
            RECORD_MATCH_RESULT => {
                let cmd: RecordMatchResult =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed RecordMatchResultCmd payload: {e}"
                        ))
                    })?;
                self.record_match_result(cmd)
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

    /// A ready-to-record standing `r-01` for player `p-self`: a consistent
    /// Glicko-2 estimate recalculated for every match played, sitting mid-tier
    /// above its floor, not a smurf, with no disconnects. Tests mutate one aspect
    /// at a time to drive a specific rejection.
    fn ready_standing() -> RankedStanding {
        let mut standing = RankedStanding::new("r-01");
        standing.set_player("p-self");
        standing.set_rating(1500.0, 200.0, 0.06);
        standing.set_match_history(6, 6);
        standing.set_ladder(Tier::Corner, 1, 0);
        standing.set_floor(Tier::Block);
        standing.set_smurf(false, false);
        standing.set_disconnects(0, 0);
        standing
    }

    /// A valid command recording a win for `p-self` in `m-42` against a 1500
    /// opponent.
    fn valid_cmd() -> RecordMatchResult {
        RecordMatchResult::new("p-self", "m-42", MatchOutcome::Win, 1500.0)
    }

    // Scenario: Successfully execute RecordMatchResultCmd — a match.result.recorded
    // event and a rating.recalculated event are both emitted.
    #[test]
    fn records_result_and_emits_both_events() {
        let mut standing = ready_standing();

        let events = standing
            .execute(valid_cmd().into_command())
            .expect("valid result should be recorded");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type(), "match.result.recorded");
        assert_eq!(events[1].event_type(), "rating.recalculated");
        match &events[0] {
            Event::MatchResultRecorded(recorded) => {
                assert_eq!(recorded.player_id, "p-self");
                assert_eq!(recorded.match_id, "m-42");
                assert_eq!(recorded.outcome, MatchOutcome::Win);
            }
            other => panic!("expected MatchResultRecorded, got {other:?}"),
        }
        match &events[1] {
            Event::RatingRecalculated(recalc) => {
                assert_eq!(recalc.player_id, "p-self");
                // A win against an equal-rated opponent nudges the rating up and
                // the RD down.
                assert!(recalc.rating > 1500.0);
                assert!(recalc.rating_deviation < 200.0);
                assert!(recalc.volatility > 0.0);
            }
            other => panic!("expected RatingRecalculated, got {other:?}"),
        }
        // Both events were recorded and the match history stayed in lockstep.
        assert_eq!(standing.version(), 2);
        assert_eq!(standing.uncommitted_events().len(), 2);
    }

    // Scenario: rejected — Glicko-2 rating, RD, and volatility are recalculated
    // after every rated match.
    #[test]
    fn rejects_when_recalculation_lags_matches() {
        let mut standing = ready_standing();
        // A prior match went unrecalculated: 7 matches, 6 recalculations.
        standing.set_match_history(7, 6);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("a lagging recalculation count must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — visible rank advances through tiers Block→Legend with 3
    // stars per tier; a win streak grants a bonus star.
    #[test]
    fn rejects_when_stars_exceed_tier_cap() {
        let mut standing = ready_standing();
        // More stars than a tier can hold — the ladder structure is corrupt.
        standing.set_ladder(Tier::Corner, STARS_PER_TIER + 1, 0);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("a star count past the per-tier cap must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — rank-floor protection prevents demotion below a reached
    // tier floor (anti-tilt applies to Block/Corner).
    #[test]
    fn rejects_when_below_reached_tier_floor() {
        let mut standing = ready_standing();
        // Reached Corner as a floor, but the standing has slipped to Block.
        standing.set_ladder(Tier::Block, 1, 0);
        standing.set_floor(Tier::Corner);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("sitting below the reached floor must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — a suspected smurf is auto-elevated to a higher bracket
    // after 20 matches.
    #[test]
    fn rejects_when_suspected_smurf_not_elevated() {
        let mut standing = ready_standing();
        // Flagged smurf, past the 20-match threshold, yet still not elevated.
        standing.set_match_history(SMURF_ELEVATION_MATCHES, SMURF_ELEVATION_MATCHES);
        standing.set_smurf(true, false);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("an un-elevated smurf past the threshold must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // Scenario: rejected — disconnect penalties escalate (doubling) on repeated
    // abandonment.
    #[test]
    fn rejects_when_disconnect_penalty_does_not_double() {
        let mut standing = ready_standing();
        // Three disconnects should carry BASE * 2^2 = 20, but only 10 is applied.
        standing.set_disconnects(3, 10);

        let err = standing
            .execute(valid_cmd().into_command())
            .expect_err("a penalty off the doubling schedule must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A command naming a different player is rejected before any invariant runs.
    #[test]
    fn rejects_command_for_a_different_player() {
        let mut standing = ready_standing();
        let cmd = RecordMatchResult::new("p-other", "m-42", MatchOutcome::Win, 1500.0);

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a command for another player must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A result without a match id cannot be recorded.
    #[test]
    fn rejects_when_match_id_is_missing() {
        let mut standing = ready_standing();
        let cmd = RecordMatchResult::new("p-self", "   ", MatchOutcome::Win, 1500.0);

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a missing match id must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // A non-positive opponent rating leaves Glicko-2 undefined.
    #[test]
    fn rejects_when_opponent_rating_is_non_positive() {
        let mut standing = ready_standing();
        let cmd = RecordMatchResult::new("p-self", "m-42", MatchOutcome::Win, 0.0);

        let err = standing
            .execute(cmd.into_command())
            .expect_err("a non-positive opponent rating must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(standing.version(), 0);
    }

    // The disconnect doubling schedule follows BASE * 2^(n-1).
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
        assert_eq!(command.name, RecordMatchResult::COMMAND);
        let decoded: RecordMatchResult = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
