//! BattlePass bounded context — a seasonal reward track in the
//! shop-and-payments context.
//!
//! A [`BattlePass`] is a single player's seasonal pass whose reward nodes unlock
//! tier by tier as XP is earned. Purchasing the pass unlocks the premium track;
//! claiming a reward node grants its payout. Five invariants govern whether the
//! premium track may be purchased or a reward node may be claimed, and every one
//! of them is re-checked when a command is requested:
//!
//! 1. **XP thresholds monotonic** — XP thresholds are monotonically increasing
//!    across tiers; a pass whose tier ladder is out of order may not be claimed.
//! 2. **Unlock in track order** — reward nodes unlock strictly in track order; a
//!    claim that skips ahead of the next unclaimed node may not be honored.
//! 3. **Premium after purchase** — the premium track is claimable only after
//!    purchase; a premium-tier claim on an unpurchased pass may not be honored.
//! 4. **Cosmetics / credits only** — the pass awards cosmetics and $MADE credits
//!    only, never gameplay power; a node wired to grant gameplay power may not be
//!    claimed.
//! 5. **Bound to one season** — a pass is bound to a single season and expires
//!    with it; a claim against an expired (or mismatched) season may not be
//!    honored.
//!
//! Two commands are implemented. [`PurchaseBattlePass`]
//! (`PurchaseBattlePassCmd`) buys the premium track for a player via a fiat
//! Order — given a playerId, seasonId, and orderId it enforces every invariant
//! and on success emits [`Event::BattlePassPurchased`]
//! (`battlepass.purchased`). [`ClaimPassReward`] (`ClaimPassRewardCmd`) claims
//! an unlocked reward node in order for a player — given a playerId, seasonId,
//! and tier it enforces every invariant and on success emits
//! [`Event::PassRewardClaimed`] (`pass.reward.claimed`). This module is
//! hand-written (it does not use `shared::stub_aggregate!`) but preserves the
//! same public surface — a [`BattlePass`] aggregate and a
//! [`BattlePassRepository`] port — so any persistence adapters compile against it
//! unchanged, exactly like its sibling [`CardPack`](crate::card_pack).

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "BattlePass";

/// The command name that claims an unlocked reward node in track order.
const CLAIM_PASS_REWARD: &str = "ClaimPassRewardCmd";

/// The command name that purchases the premium BattlePass track.
const PURCHASE_BATTLE_PASS: &str = "PurchaseBattlePassCmd";

/// The `ClaimPassRewardCmd` payload: which player is claiming, the season the
/// pass is bound to, and the tier whose reward node is claimed. Field names use
/// the shop service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ClaimPassReward::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`BattlePass::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimPassReward {
    /// The player claiming the reward; must be non-empty.
    pub player_id: String,
    /// The season the pass is bound to; must be non-empty and must name this
    /// pass's season.
    pub season_id: String,
    /// The tier whose reward node is being claimed; must be a valid (non-zero)
    /// tier.
    pub tier: u32,
}

/// Story-facing alias for the `ClaimPassRewardCmd` payload type.
pub type ClaimPassRewardCmd = ClaimPassReward;

impl ClaimPassReward {
    /// The command name this maps to.
    pub const COMMAND: &'static str = CLAIM_PASS_REWARD;

    /// Build a command claiming the reward at `tier` for `player_id` on
    /// `season_id`.
    pub fn new(player_id: impl Into<String>, season_id: impl Into<String>, tier: u32) -> Self {
        Self {
            player_id: player_id.into(),
            season_id: season_id.into(),
            tier,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`BattlePass::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ClaimPassReward is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `PurchaseBattlePassCmd` payload: which player is purchasing the premium
/// track, the season the pass is bound to, and the fiat Order that settled the
/// purchase. Field names use the shop service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`PurchaseBattlePass::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`BattlePass::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseBattlePass {
    /// The player purchasing the premium track; must be non-empty.
    pub player_id: String,
    /// The season the pass is bound to; must be non-empty and must name this
    /// pass's season.
    pub season_id: String,
    /// The fiat Order that paid for the premium track; must be non-empty.
    pub order_id: String,
}

/// Story-facing alias for the `PurchaseBattlePassCmd` payload type.
pub type PurchaseBattlePassCmd = PurchaseBattlePass;

impl PurchaseBattlePass {
    /// The command name this maps to.
    pub const COMMAND: &'static str = PURCHASE_BATTLE_PASS;

    /// Build a command purchasing the premium track for `player_id` on
    /// `season_id`, backed by the fiat Order `order_id`.
    pub fn new(
        player_id: impl Into<String>,
        season_id: impl Into<String>,
        order_id: impl Into<String>,
    ) -> Self {
        Self {
            player_id: player_id.into(),
            season_id: season_id.into(),
            order_id: order_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`BattlePass::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("PurchaseBattlePass is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The premium-track purchase, carried by [`Event::BattlePassPurchased`] and
/// thus by the emitted `battlepass.purchased` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattlePassPurchased {
    /// The player that purchased the premium track.
    pub player_id: String,
    /// The season the pass is bound to.
    pub season_id: String,
    /// The fiat Order that paid for the premium track.
    pub order_id: String,
}

/// The reward node that was claimed, carried by [`Event::PassRewardClaimed`] and
/// thus by the emitted `pass.reward.claimed` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassRewardClaimed {
    /// The player that claimed the reward.
    pub player_id: String,
    /// The season the pass is bound to.
    pub season_id: String,
    /// The tier whose reward node was claimed.
    pub tier: u32,
}

/// Domain events emitted by [`BattlePass`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The premium track was purchased for this BattlePass.
    BattlePassPurchased(BattlePassPurchased),
    /// A reward node was claimed at a tier for a player.
    PassRewardClaimed(PassRewardClaimed),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::BattlePassPurchased(_) => "battlepass.purchased",
            Event::PassRewardClaimed(_) => "pass.reward.claimed",
        }
    }
}

/// The BattlePass aggregate: one player's seasonal reward track whose nodes are
/// purchased and claimed tier by tier.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring is unchanged, while it
/// now carries the state the [`PurchaseBattlePass`] and [`ClaimPassReward`]
/// commands validate against: whether the XP thresholds are monotonically
/// increasing, whether reward nodes unlock in track order, which tier is the
/// next unclaimed node, whether the premium track is claimable only after
/// purchase, whether the premium track has been purchased, whether the node's
/// payout is cosmetics / $MADE credits only, and whether the bound season is
/// still active.
///
/// A fresh BattlePass from [`BattlePass::new`] is valid for purchase or claim at
/// its next tier: its XP ladder is monotonic, reward nodes unlock in track order,
/// the premium track is claimable only after purchase, the premium track is
/// purchased, its rewards are cosmetics / credits only, and its season is
/// active. The configuration methods below drive it to a state a command rejects,
/// exactly as
/// [`CardPack`](crate::card_pack) is built up before a command validates it.
#[derive(Debug)]
pub struct BattlePass {
    id: String,
    root: AggregateRoot,
    /// The season this pass is bound to. A claim naming a different season is
    /// rejected; the pass is bound to a single season and expires with it.
    season_id: String,
    /// Whether the XP thresholds are monotonically increasing across tiers.
    /// `false` models a tier ladder whose thresholds are out of order.
    xp_thresholds_monotonic: bool,
    /// Whether reward nodes unlock strictly in track order. `false` models a
    /// malformed track whose nodes can be claimed out of order.
    reward_nodes_unlock_in_track_order: bool,
    /// The next tier eligible to be claimed. Reward nodes unlock strictly in
    /// track order, so a claim must name exactly this tier.
    next_claimable_tier: u32,
    /// Whether premium-track rewards remain unclaimable until the pass is
    /// purchased. `false` models an invalid track that exposes premium rewards
    /// before purchase.
    premium_track_claimable_only_after_purchase: bool,
    /// Whether the premium track has been purchased. A premium-tier claim on an
    /// unpurchased pass is rejected.
    premium_purchased: bool,
    /// Whether the reward node awards cosmetics / $MADE credits only. `false`
    /// models a node wired to grant gameplay power, which is never allowed.
    awards_cosmetics_or_credits_only: bool,
    /// Whether the bound season is still active (not expired). A claim against an
    /// expired season is rejected.
    season_active: bool,
    /// The first tier that belongs to the premium track. Claims at or above this
    /// tier require [`Self::premium_purchased`].
    premium_track_start: u32,
}

impl BattlePass {
    /// Create a new, claimable BattlePass identified by `id` and bound to
    /// `season_id`: its XP ladder is monotonic, its next claimable tier is 1, its
    /// reward nodes unlock in track order, its premium track is claimable only
    /// after purchase, its premium track is purchased, its rewards are cosmetics
    /// / credits only, and its season is active. Use the configuration methods to
    /// drive it to the state a command validates.
    pub fn new(id: impl Into<String>, season_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            season_id: season_id.into(),
            xp_thresholds_monotonic: true,
            reward_nodes_unlock_in_track_order: true,
            next_claimable_tier: 1,
            premium_track_claimable_only_after_purchase: true,
            premium_purchased: true,
            awards_cosmetics_or_credits_only: true,
            season_active: true,
            premium_track_start: 1,
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The season this pass is bound to.
    pub fn season_id(&self) -> &str {
        &self.season_id
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// Record whether the XP thresholds are monotonically increasing across tiers
    /// (`false` models an out-of-order tier ladder).
    pub fn set_xp_thresholds_monotonic(&mut self, ok: bool) {
        self.xp_thresholds_monotonic = ok;
    }

    /// Record whether reward nodes unlock strictly in track order (`false`
    /// models a malformed track whose nodes can unlock out of order).
    pub fn set_reward_nodes_unlock_in_track_order(&mut self, ok: bool) {
        self.reward_nodes_unlock_in_track_order = ok;
    }

    /// Set the next tier eligible to be claimed in track order.
    pub fn set_next_claimable_tier(&mut self, tier: u32) {
        self.next_claimable_tier = tier;
    }

    /// Record whether premium-track rewards remain unclaimable until purchase
    /// (`false` models an invalid track exposing premium rewards pre-purchase).
    pub fn set_premium_track_claimable_only_after_purchase(&mut self, ok: bool) {
        self.premium_track_claimable_only_after_purchase = ok;
    }

    /// Record whether the premium track has been purchased.
    pub fn set_premium_purchased(&mut self, purchased: bool) {
        self.premium_purchased = purchased;
    }

    /// Set the first tier belonging to the premium track.
    pub fn set_premium_track_start(&mut self, tier: u32) {
        self.premium_track_start = tier;
    }

    /// Record whether the reward node awards cosmetics / $MADE credits only
    /// (`false` models a node wired to grant gameplay power).
    pub fn set_awards_cosmetics_or_credits_only(&mut self, ok: bool) {
        self.awards_cosmetics_or_credits_only = ok;
    }

    /// Record whether the bound season is still active (`false` models an expired
    /// season).
    pub fn set_season_active(&mut self, active: bool) {
        self.season_active = active;
    }

    /// Monotonicity invariant: XP thresholds are monotonically increasing across
    /// tiers.
    fn ensure_xp_thresholds_monotonic(&self) -> Result<(), DomainError> {
        if !self.xp_thresholds_monotonic {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' has non-monotonic XP thresholds; XP thresholds are monotonically \
                 increasing across tiers",
                self.id
            )));
        }
        Ok(())
    }

    /// Track-order invariant: reward nodes unlock strictly in track order.
    fn ensure_reward_nodes_unlock_in_track_order(&self) -> Result<(), DomainError> {
        if !self.reward_nodes_unlock_in_track_order {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' reward nodes can unlock out of order; reward nodes unlock \
                 strictly in track order",
                self.id
            )));
        }
        Ok(())
    }

    /// Track-order invariant: reward nodes unlock strictly in track order, so the
    /// claim must name exactly the next unclaimed tier.
    fn ensure_unlocks_in_track_order(&self, tier: u32) -> Result<(), DomainError> {
        self.ensure_reward_nodes_unlock_in_track_order()?;
        if tier != self.next_claimable_tier {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' cannot claim tier {tier}; the next claimable tier is {}, and \
                 reward nodes unlock strictly in track order",
                self.id, self.next_claimable_tier
            )));
        }
        Ok(())
    }

    /// Premium invariant: the premium track is claimable only after purchase.
    fn ensure_premium_track_claimable_only_after_purchase(&self) -> Result<(), DomainError> {
        if !self.premium_track_claimable_only_after_purchase {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' exposes premium rewards before purchase; the premium track is \
                 claimable only after purchase",
                self.id
            )));
        }
        Ok(())
    }

    /// Premium invariant: the premium track is claimable only after purchase.
    fn ensure_premium_claimable(&self, tier: u32) -> Result<(), DomainError> {
        self.ensure_premium_track_claimable_only_after_purchase()?;
        if tier >= self.premium_track_start && !self.premium_purchased {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' cannot claim premium tier {tier}; the premium track is claimable \
                 only after purchase",
                self.id
            )));
        }
        Ok(())
    }

    /// Payout invariant: the pass awards cosmetics and $MADE credits only — never
    /// gameplay power.
    fn ensure_awards_cosmetics_or_credits_only(&self) -> Result<(), DomainError> {
        if !self.awards_cosmetics_or_credits_only {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' reward node grants gameplay power; the pass awards cosmetics and \
                 $MADE credits only — never gameplay power",
                self.id
            )));
        }
        Ok(())
    }

    /// Season-binding invariant: a pass is bound to a single season and expires
    /// with it.
    fn ensure_season_active(&self) -> Result<(), DomainError> {
        if !self.season_active {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' season '{}' has expired; a pass is bound to a single season and \
                 expires with it",
                self.id, self.season_id
            )));
        }
        Ok(())
    }

    /// Handle `PurchaseBattlePassCmd`: verify the command carries a valid
    /// playerId, a valid seasonId (naming this pass's season), and a valid
    /// orderId; enforce every invariant (XP thresholds monotonic, unlock-in-
    /// track-order, premium claimability gated by purchase, cosmetics / credits
    /// only, and season bound / active); mark the premium track purchased; and
    /// emit [`Event::BattlePassPurchased`].
    fn purchase_battle_pass(&mut self, cmd: PurchaseBattlePass) -> Result<Vec<Event>, DomainError> {
        // A valid playerId must be supplied.
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' requires a valid playerId to purchase the premium track",
                self.id
            )));
        }
        // A valid seasonId must be supplied.
        if cmd.season_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' requires a valid seasonId to purchase the premium track",
                self.id
            )));
        }
        // The command must name the season this pass is bound to — a pass is
        // bound to a single season.
        if cmd.season_id != self.season_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets season '{}' but battle pass '{}' is bound to season '{}'; a pass \
                 is bound to a single season and expires with it",
                cmd.season_id, self.id, self.season_id
            )));
        }
        // A valid orderId must be supplied. The Order aggregate owns payment
        // confirmation; BattlePass records only the fiat Order reference.
        if cmd.order_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' requires a valid orderId to purchase the premium track",
                self.id
            )));
        }

        // Enforce every invariant before recording the purchase.
        self.ensure_xp_thresholds_monotonic()?;
        self.ensure_reward_nodes_unlock_in_track_order()?;
        self.ensure_premium_track_claimable_only_after_purchase()?;
        self.ensure_awards_cosmetics_or_credits_only()?;
        self.ensure_season_active()?;

        self.premium_purchased = true;

        let event = Event::BattlePassPurchased(BattlePassPurchased {
            player_id: cmd.player_id,
            season_id: cmd.season_id,
            order_id: cmd.order_id,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `ClaimPassRewardCmd`: verify the command carries a valid playerId, a
    /// valid seasonId (naming this pass's season), and a valid tier; enforce every
    /// invariant (XP thresholds monotonic, unlock-in-track-order, premium after
    /// purchase, cosmetics / credits only, and season bound / active); advance the
    /// next claimable tier; and emit [`Event::PassRewardClaimed`].
    fn claim_pass_reward(&mut self, cmd: ClaimPassReward) -> Result<Vec<Event>, DomainError> {
        // A valid playerId must be supplied.
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' requires a valid playerId to claim a reward",
                self.id
            )));
        }
        // A valid seasonId must be supplied.
        if cmd.season_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' requires a valid seasonId to claim a reward",
                self.id
            )));
        }
        // The command must name the season this pass is bound to — a pass is
        // bound to a single season.
        if cmd.season_id != self.season_id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets season '{}' but battle pass '{}' is bound to season '{}'; a pass \
                 is bound to a single season and expires with it",
                cmd.season_id, self.id, self.season_id
            )));
        }
        // A valid (non-zero) tier must be supplied.
        if cmd.tier == 0 {
            return Err(DomainError::InvariantViolation(format!(
                "battle pass '{}' requires a valid (non-zero) tier to claim a reward",
                self.id
            )));
        }

        // Enforce every invariant before recording the claim.
        self.ensure_xp_thresholds_monotonic()?;
        self.ensure_unlocks_in_track_order(cmd.tier)?;
        self.ensure_premium_claimable(cmd.tier)?;
        self.ensure_awards_cosmetics_or_credits_only()?;
        self.ensure_season_active()?;

        // Advance the track so the next claim must name the following node, keeping
        // reward nodes unlocking strictly in track order.
        self.next_claimable_tier = cmd.tier + 1;

        let event = Event::PassRewardClaimed(PassRewardClaimed {
            player_id: cmd.player_id,
            season_id: cmd.season_id,
            tier: cmd.tier,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for BattlePass {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            PURCHASE_BATTLE_PASS => {
                let cmd: PurchaseBattlePass =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed PurchaseBattlePassCmd payload: {e}"
                        ))
                    })?;
                self.purchase_battle_pass(cmd)
            }
            CLAIM_PASS_REWARD => {
                let cmd: ClaimPassReward =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ClaimPassRewardCmd payload: {e}"
                        ))
                    })?;
                self.claim_pass_reward(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`BattlePass`] aggregate. Adapters implement
/// [`shared::Repository`] for `BattlePass` and then this marker trait.
pub trait BattlePassRepository: Repository<BattlePass> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A claimable BattlePass `bp-01` bound to season `s-01`: monotonic XP ladder,
    /// next claimable tier 1, premium purchased, cosmetics / credits only, season
    /// active. Tests mutate one aspect at a time to drive a specific rejection.
    fn ready_pass() -> BattlePass {
        let mut pass = BattlePass::new("bp-01", "s-01");
        pass.set_xp_thresholds_monotonic(true);
        pass.set_reward_nodes_unlock_in_track_order(true);
        pass.set_next_claimable_tier(1);
        pass.set_premium_track_claimable_only_after_purchase(true);
        pass.set_premium_purchased(true);
        pass.set_awards_cosmetics_or_credits_only(true);
        pass.set_season_active(true);
        pass
    }

    /// A valid, not-yet-purchased BattlePass: its premium track is correctly
    /// gated until purchase, and the purchase command may unlock it.
    fn ready_unpurchased_pass() -> BattlePass {
        let mut pass = ready_pass();
        pass.set_premium_purchased(false);
        pass
    }

    /// A command claiming the tier-1 reward on pass `bp-01` for player `p-01` on
    /// season `s-01`.
    fn valid_cmd() -> ClaimPassReward {
        ClaimPassReward::new("p-01", "s-01", 1)
    }

    /// A command purchasing the premium track for player `p-01` in season `s-01`
    /// using fiat Order `o-01`.
    fn valid_purchase_cmd() -> PurchaseBattlePass {
        PurchaseBattlePass::new("p-01", "s-01", "o-01")
    }

    // Scenario: Successfully execute PurchaseBattlePassCmd.
    #[test]
    fn purchases_and_emits_battlepass_purchased_event() {
        let mut pass = ready_unpurchased_pass();

        let events = pass
            .execute(valid_purchase_cmd().into_command())
            .expect("valid purchase should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "battlepass.purchased");
        match &events[0] {
            Event::BattlePassPurchased(purchased) => {
                assert_eq!(purchased.player_id, "p-01");
                assert_eq!(purchased.season_id, "s-01");
                assert_eq!(purchased.order_id, "o-01");
            }
            other => panic!("expected BattlePassPurchased, got {other:?}"),
        }
        // The BattlePass recorded the event and the premium track can now be
        // claimed.
        assert_eq!(pass.version(), 1);
        assert_eq!(pass.uncommitted_events().len(), 1);
        assert_eq!(
            pass.uncommitted_events()[0].event_type(),
            "battlepass.purchased"
        );
        pass.execute(valid_cmd().into_command())
            .expect("purchase should unlock the premium track");
        assert_eq!(pass.version(), 2);
    }

    // Scenario: PurchaseBattlePassCmd rejected — XP thresholds are monotonically
    // increasing across tiers.
    #[test]
    fn purchase_rejects_when_xp_thresholds_not_monotonic() {
        let mut pass = ready_unpurchased_pass();
        // The tier ladder's XP thresholds are out of order.
        pass.set_xp_thresholds_monotonic(false);

        let err = pass
            .execute(valid_purchase_cmd().into_command())
            .expect_err("a non-monotonic XP ladder must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Scenario: PurchaseBattlePassCmd rejected — Reward nodes unlock strictly in
    // track order.
    #[test]
    fn purchase_rejects_when_reward_nodes_do_not_unlock_in_track_order() {
        let mut pass = ready_unpurchased_pass();
        // The reward track is malformed and can unlock nodes out of order.
        pass.set_reward_nodes_unlock_in_track_order(false);

        let err = pass
            .execute(valid_purchase_cmd().into_command())
            .expect_err("an out-of-order reward track must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Scenario: PurchaseBattlePassCmd rejected — The premium track is claimable
    // only after purchase.
    #[test]
    fn purchase_rejects_when_premium_track_is_claimable_before_purchase() {
        let mut pass = ready_unpurchased_pass();
        // The premium track is incorrectly exposed before purchase.
        pass.set_premium_track_claimable_only_after_purchase(false);

        let err = pass
            .execute(valid_purchase_cmd().into_command())
            .expect_err("premium rewards exposed before purchase must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Scenario: PurchaseBattlePassCmd rejected — The pass awards cosmetics and
    // $MADE credits only — never gameplay power.
    #[test]
    fn purchase_rejects_when_node_grants_gameplay_power() {
        let mut pass = ready_unpurchased_pass();
        // The reward node is wired to grant gameplay power.
        pass.set_awards_cosmetics_or_credits_only(false);

        let err = pass
            .execute(valid_purchase_cmd().into_command())
            .expect_err("a node granting gameplay power must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Scenario: PurchaseBattlePassCmd rejected — A pass is bound to a single
    // season and expires with it.
    #[test]
    fn purchase_rejects_when_season_expired() {
        let mut pass = ready_unpurchased_pass();
        // The season the pass is bound to has expired.
        pass.set_season_active(false);

        let err = pass
            .execute(valid_purchase_cmd().into_command())
            .expect_err("a purchase against an expired season must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // A PurchaseBattlePassCmd naming a different season is rejected before any
    // invariant runs.
    #[test]
    fn purchase_rejects_command_for_a_different_season() {
        let mut pass = ready_unpurchased_pass();
        let cmd = PurchaseBattlePass::new("p-01", "s-99", "o-01");

        let err = pass
            .execute(cmd.into_command())
            .expect_err("a purchase for another season must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Commands missing required fields are rejected.
    #[test]
    fn purchase_rejects_command_with_missing_fields() {
        for cmd in [
            PurchaseBattlePass::new("   ", "s-01", "o-01"),
            PurchaseBattlePass::new("p-01", "   ", "o-01"),
            PurchaseBattlePass::new("p-01", "s-01", "   "),
        ] {
            let mut pass = ready_unpurchased_pass();
            let err = pass
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(pass.version(), 0);
        }
    }

    #[test]
    fn purchase_command_payload_round_trips() {
        let cmd = valid_purchase_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, PurchaseBattlePass::COMMAND);
        let decoded: PurchaseBattlePass = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_purchase_cmd());
    }

    // Scenario: Successfully execute ClaimPassRewardCmd.
    #[test]
    fn claims_and_emits_pass_reward_claimed_event() {
        let mut pass = ready_pass();

        let events = pass
            .execute(valid_cmd().into_command())
            .expect("valid claim should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "pass.reward.claimed");
        match &events[0] {
            Event::PassRewardClaimed(claimed) => {
                assert_eq!(claimed.player_id, "p-01");
                assert_eq!(claimed.season_id, "s-01");
                assert_eq!(claimed.tier, 1);
            }
            other => panic!("expected PassRewardClaimed, got {other:?}"),
        }
        // The BattlePass recorded the event and advanced the track.
        assert_eq!(pass.version(), 1);
        assert_eq!(pass.uncommitted_events().len(), 1);
        assert_eq!(
            pass.uncommitted_events()[0].event_type(),
            "pass.reward.claimed"
        );
    }

    // Scenario: rejected — XP thresholds are monotonically increasing across tiers.
    #[test]
    fn rejects_when_xp_thresholds_not_monotonic() {
        let mut pass = ready_pass();
        // The tier ladder's XP thresholds are out of order.
        pass.set_xp_thresholds_monotonic(false);

        let err = pass
            .execute(valid_cmd().into_command())
            .expect_err("a non-monotonic XP ladder must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Scenario: rejected — Reward nodes unlock strictly in track order.
    #[test]
    fn rejects_when_claim_skips_track_order() {
        let mut pass = ready_pass();
        // The next claimable node is tier 1, but the command skips ahead to tier 3.
        let cmd = ClaimPassReward::new("p-01", "s-01", 3);

        let err = pass
            .execute(cmd.into_command())
            .expect_err("a claim that skips ahead in the track must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Scenario: rejected — The premium track is claimable only after purchase.
    #[test]
    fn rejects_premium_claim_before_purchase() {
        let mut pass = ready_pass();
        // The premium track begins at tier 1 and has not been purchased.
        pass.set_premium_track_start(1);
        pass.set_premium_purchased(false);

        let err = pass
            .execute(valid_cmd().into_command())
            .expect_err("a premium claim on an unpurchased pass must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Scenario: rejected — The pass awards cosmetics and $MADE credits only — never
    // gameplay power.
    #[test]
    fn rejects_when_node_grants_gameplay_power() {
        let mut pass = ready_pass();
        // The reward node is wired to grant gameplay power.
        pass.set_awards_cosmetics_or_credits_only(false);

        let err = pass
            .execute(valid_cmd().into_command())
            .expect_err("a node granting gameplay power must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Scenario: rejected — A pass is bound to a single season and expires with it.
    #[test]
    fn rejects_when_season_expired() {
        let mut pass = ready_pass();
        // The season the pass is bound to has expired.
        pass.set_season_active(false);

        let err = pass
            .execute(valid_cmd().into_command())
            .expect_err("a claim against an expired season must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // A claim naming a different season is rejected — a pass is bound to a single
    // season.
    #[test]
    fn rejects_claim_for_a_different_season() {
        let mut pass = ready_pass();
        let cmd = ClaimPassReward::new("p-01", "s-99", 1);

        let err = pass
            .execute(cmd.into_command())
            .expect_err("a claim for another season must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Commands missing the required playerId are rejected.
    #[test]
    fn rejects_command_with_missing_player_id() {
        let mut pass = ready_pass();
        let err = pass
            .execute(ClaimPassReward::new("   ", "s-01", 1).into_command())
            .expect_err("a command with a missing playerId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Commands missing the required seasonId are rejected.
    #[test]
    fn rejects_command_with_missing_season_id() {
        let mut pass = ready_pass();
        let err = pass
            .execute(ClaimPassReward::new("p-01", "   ", 1).into_command())
            .expect_err("a command with a missing seasonId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Commands with an invalid (zero) tier are rejected.
    #[test]
    fn rejects_command_with_invalid_tier() {
        let mut pass = ready_pass();
        let err = pass
            .execute(ClaimPassReward::new("p-01", "s-01", 0).into_command())
            .expect_err("a command with a zero tier must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pass.version(), 0);
    }

    // Claiming in order across tiers advances the track node by node.
    #[test]
    fn claims_advance_the_track_in_order() {
        let mut pass = ready_pass();

        pass.execute(ClaimPassReward::new("p-01", "s-01", 1).into_command())
            .expect("first claim should succeed");
        pass.execute(ClaimPassReward::new("p-01", "s-01", 2).into_command())
            .expect("next claim should succeed");

        assert_eq!(pass.version(), 2);
        // A repeated claim of an already-claimed node is rejected — nodes unlock
        // strictly in track order.
        let err = pass
            .execute(ClaimPassReward::new("p-01", "s-01", 1).into_command())
            .expect_err("re-claiming an already-claimed node must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut pass = BattlePass::new("bp-01", "s-01");
        let err = pass.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "BattlePass");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, ClaimPassReward::COMMAND);
        let decoded: ClaimPassReward = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }
}
