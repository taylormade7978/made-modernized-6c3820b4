//! EmissionPool bounded context - the $MADE reward pool that seasons draw down,
//! and its low-pool early-warning signal, in the token-and-marketplace context.
//!
//! An [`EmissionPool`] holds the season's $MADE reward budget, emits rewards to
//! recipients within its remaining balance, and raises a low-pool warning ahead
//! of exhaustion. Four invariants govern whether either operation is allowed:
//!
//! 1. **Emission schedule** - Season 1 opens with a 30M pool and each subsequent
//!    season's pool follows the mandated halving schedule; a pool whose schedule
//!    does not match is rejected.
//! 2. **Balance ceiling** - the pool can never emit more than its remaining
//!    balance; an emission that would overdraw the pool, or a warning raised on
//!    an insolvent pool, is rejected.
//! 3. **Low-pool warning** - the 50% low-pool warning must be raised at 80%
//!    depletion (advance notice), not at exhaustion; a pool whose warning
//!    threshold is misconfigured is rejected.
//! 4. **Governance gating** - pool size is a governance-adjustable parameter and
//!    a later season cannot open until the prior season's drain is understood; a
//!    pool whose governance gate is unresolved is rejected.
//!
//! [`EmitRewardCmd`] (`EmitRewardCmd`) validates the poolId, recipientId, and
//! amount, enforces every invariant, deducts the emitted amount from the
//! remaining balance, and on success emits [`Event::RewardEmitted`]
//! (`reward.emitted`). [`RaiseLowPoolWarningCmd`] (`RaiseLowPoolWarningCmd`)
//! validates the poolId and depletionPct, enforces the same invariants, and on
//! success emits [`Event::LowPoolWarningRaised`] (`low.pool.warning.raised`).
//! This module is hand-written (it does not use `shared::stub_aggregate!`) but
//! preserves the same public surface as the scaffolded contexts: an
//! [`EmissionPool`] aggregate and an [`EmissionPoolRepository`] port.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "EmissionPool";

/// The command name [`EmissionPool::execute`] recognizes to emit $MADE from the
/// pool to a recipient.
const EMIT_REWARD: &str = "EmitRewardCmd";

/// The command name [`EmissionPool::execute`] recognizes to raise the 50%
/// low-pool warning at 80% depletion.
const RAISE_LOW_POOL_WARNING: &str = "RaiseLowPoolWarningCmd";

/// The $MADE balance Season 1's pool opens with (30M base units). A freshly
/// constructed [`EmissionPool`] starts fully funded at this amount.
const SEASON_ONE_POOL: u64 = 30_000_000;

/// The `EmitRewardCmd` payload: which pool to draw from, who receives the
/// reward, and how much $MADE to emit. Field names use the token marketplace's
/// `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`EmitRewardCmd::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`EmissionPool::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmitRewardCmd {
    /// The pool being drawn from; must name this EmissionPool.
    pub pool_id: String,
    /// The recipient receiving the emitted reward; must be a valid identifier.
    pub recipient_id: String,
    /// The amount of $MADE to emit, in base units; must be positive and within
    /// the pool's remaining balance.
    pub amount: u64,
}

impl EmitRewardCmd {
    /// The command name this maps to.
    pub const COMMAND: &'static str = EMIT_REWARD;

    /// Build a command emitting `amount` $MADE from `pool_id` to `recipient_id`.
    pub fn new(pool_id: impl Into<String>, recipient_id: impl Into<String>, amount: u64) -> Self {
        Self {
            pool_id: pool_id.into(),
            recipient_id: recipient_id.into(),
            amount,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`EmissionPool::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("EmitRewardCmd is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `RaiseLowPoolWarningCmd` payload: which pool to warn on and the depletion
/// percentage the warning is raised at. Field names use the token
/// marketplace's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`RaiseLowPoolWarningCmd::into_command`], or decode it from a command payload
/// via [`serde_json`] inside [`EmissionPool::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RaiseLowPoolWarningCmd {
    /// The emission pool being warned on; must name this EmissionPool.
    pub pool_id: String,
    /// The depletion percentage the warning is raised at; must be in `0..=100`.
    pub depletion_pct: u8,
}

impl RaiseLowPoolWarningCmd {
    /// The command name this maps to.
    pub const COMMAND: &'static str = RAISE_LOW_POOL_WARNING;

    /// Build a command raising a low-pool warning on `pool_id` at
    /// `depletion_pct` percent depletion.
    pub fn new(pool_id: impl Into<String>, depletion_pct: u8) -> Self {
        Self {
            pool_id: pool_id.into(),
            depletion_pct,
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`EmissionPool::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload =
            serde_json::to_vec(self).expect("RaiseLowPoolWarningCmd is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The reward that was emitted, carried by [`Event::RewardEmitted`] and thus by
/// the emitted `reward.emitted` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewardEmitted {
    /// The pool the reward was drawn from.
    pub pool_id: String,
    /// The recipient that received the emitted reward.
    pub recipient_id: String,
    /// The amount of $MADE emitted, in base units.
    pub amount: u64,
    /// The pool's remaining balance after the emission was applied.
    pub remaining_balance: u64,
}

/// The low-pool warning that was raised, carried by
/// [`Event::LowPoolWarningRaised`] and thus by the emitted
/// `low.pool.warning.raised` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowPoolWarningRaised {
    /// The emission pool the warning was raised on.
    pub pool_id: String,
    /// The depletion percentage the warning was raised at.
    pub depletion_pct: u8,
}

/// Domain events emitted by [`EmissionPool`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// $MADE was emitted from the pool to a recipient.
    RewardEmitted(RewardEmitted),
    /// The 50% low-pool warning was raised at 80% depletion.
    LowPoolWarningRaised(LowPoolWarningRaised),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::RewardEmitted(_) => "reward.emitted",
            Event::LowPoolWarningRaised(_) => "low.pool.warning.raised",
        }
    }
}

/// The EmissionPool aggregate: the $MADE reward budget a season draws down, and
/// its low-pool early-warning signal.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so surrounding wiring stays consistent, while it
/// carries the state its commands validate: the remaining balance, whether the
/// emission schedule matches the mandated halving, whether the low-pool warning
/// threshold is configured, and whether the governance gate is resolved.
#[derive(Debug)]
pub struct EmissionPool {
    id: String,
    root: AggregateRoot,
    /// The $MADE remaining in the pool; an emission may never exceed it.
    remaining_balance: u64,
    /// Whether the pool's emission schedule matches the mandated season
    /// schedule (Season 1 = 30M, each later season halved per schedule).
    emission_schedule_valid: bool,
    /// Whether the low-pool warning is configured to raise at 80% depletion
    /// (advance notice), rather than at exhaustion.
    low_pool_warning_valid: bool,
    /// Whether the governance gate is resolved: a later season cannot open until
    /// the prior season's drain is understood.
    governance_schedule_valid: bool,
}

impl EmissionPool {
    /// Create a new, emission-ready EmissionPool identified by `id`: fully funded
    /// at the Season 1 pool of 30M $MADE, with a valid emission schedule, a
    /// correctly configured low-pool warning, and a resolved governance gate. Use
    /// the configuration methods to drive it to the state a command validates.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            remaining_balance: SEASON_ONE_POOL,
            emission_schedule_valid: true,
            low_pool_warning_valid: true,
            governance_schedule_valid: true,
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

    /// The $MADE remaining in the pool.
    pub fn remaining_balance(&self) -> u64 {
        self.remaining_balance
    }

    /// Set the pool's remaining balance (used to model a depleted or partially
    /// drained pool).
    pub fn set_remaining_balance(&mut self, balance: u64) {
        self.remaining_balance = balance;
    }

    /// Record whether the pool's emission schedule matches the mandated season
    /// schedule (`false` models a pool whose schedule diverges from the 30M
    /// Season 1 / halving progression).
    pub fn set_emission_schedule_valid(&mut self, valid: bool) {
        self.emission_schedule_valid = valid;
    }

    /// Record whether the low-pool warning threshold is configured correctly
    /// (`false` models a warning raised at exhaustion instead of at 80%
    /// depletion).
    pub fn set_low_pool_warning_valid(&mut self, valid: bool) {
        self.low_pool_warning_valid = valid;
    }

    /// Record whether the governance gate is resolved (`false` models a pool
    /// whose next season cannot open because the prior drain is not understood).
    pub fn set_governance_schedule_valid(&mut self, valid: bool) {
        self.governance_schedule_valid = valid;
    }

    /// Emission-schedule invariant: Season 1 opens with a 30M pool and each
    /// subsequent season's pool follows the mandated halving schedule.
    fn ensure_emission_schedule_valid(&self) -> Result<(), DomainError> {
        if !self.emission_schedule_valid {
            return Err(DomainError::InvariantViolation(format!(
                "emission pool '{}' emission schedule does not match the mandated season \
                 schedule (Season 1 opens with a 30M pool; each subsequent season halves per \
                 the prior schedule)",
                self.id
            )));
        }
        Ok(())
    }

    /// Balance invariant: the pool can never emit more than its remaining
    /// balance.
    fn ensure_within_remaining_balance(&self, amount: u64) -> Result<(), DomainError> {
        if amount > self.remaining_balance {
            return Err(DomainError::InvariantViolation(format!(
                "emission pool '{}' cannot emit {amount} $MADE; the pool can never emit more \
                 than its remaining balance of {}",
                self.id, self.remaining_balance
            )));
        }
        Ok(())
    }

    /// Balance invariant for a low-pool warning: the pool can never emit more
    /// than its remaining balance, so a warning raised on an insolvent pool (no
    /// remaining balance to back further emissions) is rejected.
    fn ensure_pool_solvent(&self) -> Result<(), DomainError> {
        if self.remaining_balance == 0 {
            return Err(DomainError::InvariantViolation(format!(
                "emission pool '{}' has no remaining balance; the pool can never emit more than \
                 its remaining balance",
                self.id
            )));
        }
        Ok(())
    }

    /// Low-pool-warning invariant: the 50% warning must be raised at 80%
    /// depletion (advance notice), not at exhaustion.
    fn ensure_low_pool_warning_valid(&self) -> Result<(), DomainError> {
        if !self.low_pool_warning_valid {
            return Err(DomainError::InvariantViolation(format!(
                "emission pool '{}' low-pool warning is misconfigured; the 50% low-pool warning \
                 must be raised at 80% depletion (advance notice), not at exhaustion",
                self.id
            )));
        }
        Ok(())
    }

    /// Governance invariant: pool size is a governance-adjustable parameter and a
    /// later season cannot open until the prior season's drain is understood.
    fn ensure_governance_schedule_valid(&self) -> Result<(), DomainError> {
        if !self.governance_schedule_valid {
            return Err(DomainError::InvariantViolation(format!(
                "emission pool '{}' governance gate is unresolved; pool size is a \
                 governance-adjustable parameter and a later season cannot open until the \
                 prior season's drain is understood",
                self.id
            )));
        }
        Ok(())
    }

    /// Handle `EmitRewardCmd`: verify the command carries a valid poolId (naming
    /// this EmissionPool), recipientId, and amount; enforce every emission-pool
    /// invariant; deduct the emitted amount from the remaining balance; and emit
    /// [`Event::RewardEmitted`].
    fn emit_reward(&mut self, cmd: EmitRewardCmd) -> Result<Vec<Event>, DomainError> {
        if cmd.pool_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "emission pool '{}' requires a valid poolId to emit a reward",
                self.id
            )));
        }
        if cmd.recipient_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "emission pool '{}' requires a valid recipientId to emit a reward",
                self.id
            )));
        }
        if cmd.amount == 0 {
            return Err(DomainError::InvariantViolation(format!(
                "emission pool '{}' requires a positive amount to emit a reward",
                self.id
            )));
        }
        if cmd.pool_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets emission pool '{}' but this aggregate is emission pool '{}'",
                cmd.pool_id, self.id
            )));
        }

        self.ensure_emission_schedule_valid()?;
        self.ensure_within_remaining_balance(cmd.amount)?;
        self.ensure_low_pool_warning_valid()?;
        self.ensure_governance_schedule_valid()?;

        // Draw the emission down from the pool; the ceiling check above
        // guarantees this never underflows.
        self.remaining_balance -= cmd.amount;

        let event = Event::RewardEmitted(RewardEmitted {
            pool_id: cmd.pool_id,
            recipient_id: cmd.recipient_id,
            amount: cmd.amount,
            remaining_balance: self.remaining_balance,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `RaiseLowPoolWarningCmd`: verify the command carries a valid poolId
    /// (naming this EmissionPool) and a depletionPct in range; enforce every
    /// emission invariant; and emit [`Event::LowPoolWarningRaised`].
    fn raise_low_pool_warning(
        &mut self,
        cmd: RaiseLowPoolWarningCmd,
    ) -> Result<Vec<Event>, DomainError> {
        if cmd.pool_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "emission pool '{}' requires a valid poolId to raise a low-pool warning",
                self.id
            )));
        }
        if cmd.depletion_pct > 100 {
            return Err(DomainError::InvariantViolation(format!(
                "emission pool '{}' requires a depletionPct in the range 0..=100 to raise a \
                 low-pool warning, got {}",
                self.id, cmd.depletion_pct
            )));
        }
        if cmd.pool_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets emission pool '{}' but this aggregate is emission pool '{}'",
                cmd.pool_id, self.id
            )));
        }

        self.ensure_emission_schedule_valid()?;
        self.ensure_pool_solvent()?;
        self.ensure_low_pool_warning_valid()?;
        self.ensure_governance_schedule_valid()?;

        let event = Event::LowPoolWarningRaised(LowPoolWarningRaised {
            pool_id: cmd.pool_id,
            depletion_pct: cmd.depletion_pct,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for EmissionPool {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            EMIT_REWARD => {
                let cmd: EmitRewardCmd = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed EmitRewardCmd payload: {e}"))
                })?;
                self.emit_reward(cmd)
            }
            RAISE_LOW_POOL_WARNING => {
                let cmd: RaiseLowPoolWarningCmd = serde_json::from_slice(&command.payload)
                    .map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed RaiseLowPoolWarningCmd payload: {e}"
                        ))
                    })?;
                self.raise_low_pool_warning(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`EmissionPool`] aggregate. Adapters implement
/// [`shared::Repository`] for `EmissionPool` and then this marker trait.
pub trait EmissionPoolRepository: Repository<EmissionPool> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The depletion percentage at which the advance-notice low-pool warning is
    /// raised: 80% depletion, not exhaustion (100%).
    const WARNING_DEPLETION_PCT: u8 = 80;

    /// An emission-ready EmissionPool `pool-01`: fully funded at the Season 1
    /// pool of 30M $MADE, with a valid emission schedule, a correctly configured
    /// low-pool warning, and a resolved governance gate.
    fn ready_pool() -> EmissionPool {
        let mut pool = EmissionPool::new("pool-01");
        pool.set_remaining_balance(SEASON_ONE_POOL);
        pool.set_emission_schedule_valid(true);
        pool.set_low_pool_warning_valid(true);
        pool.set_governance_schedule_valid(true);
        pool
    }

    /// A command emitting 1000 $MADE from `pool-01` to `recipient-7`.
    fn valid_cmd() -> EmitRewardCmd {
        EmitRewardCmd::new("pool-01", "recipient-7", 1000)
    }

    /// A command raising a low-pool warning on `pool-01` at 80% depletion.
    fn valid_warning_cmd() -> RaiseLowPoolWarningCmd {
        RaiseLowPoolWarningCmd::new("pool-01", WARNING_DEPLETION_PCT)
    }

    // Scenario: Successfully execute EmitRewardCmd.
    #[test]
    fn emits_and_records_reward_emitted_event() {
        let mut pool = ready_pool();

        let events = pool
            .execute(valid_cmd().into_command())
            .expect("valid emission should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "reward.emitted");
        match &events[0] {
            Event::RewardEmitted(emitted) => {
                assert_eq!(emitted.pool_id, "pool-01");
                assert_eq!(emitted.recipient_id, "recipient-7");
                assert_eq!(emitted.amount, 1000);
                assert_eq!(emitted.remaining_balance, SEASON_ONE_POOL - 1000);
            }
            other => panic!("expected RewardEmitted, got {other:?}"),
        }
        // The emission was drawn down from the pool.
        assert_eq!(pool.remaining_balance(), SEASON_ONE_POOL - 1000);
        // The EmissionPool recorded the event and advanced its version.
        assert_eq!(pool.version(), 1);
        assert_eq!(pool.uncommitted_events().len(), 1);
        assert_eq!(pool.uncommitted_events()[0].event_type(), "reward.emitted");
    }

    // Scenario: rejected - Season 1 opens with a 30M pool; each subsequent
    // season's pool halves by the mandated schedule.
    #[test]
    fn rejects_when_emission_schedule_is_invalid() {
        let mut pool = ready_pool();
        pool.set_emission_schedule_valid(false);

        let err = pool
            .execute(valid_cmd().into_command())
            .expect_err("an invalid emission schedule must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
        // A rejected emission leaves the balance untouched.
        assert_eq!(pool.remaining_balance(), SEASON_ONE_POOL);
    }

    // Scenario: rejected - The pool can never emit more than its remaining
    // balance.
    #[test]
    fn rejects_when_amount_exceeds_remaining_balance() {
        let mut pool = ready_pool();
        pool.set_remaining_balance(500);

        let err = pool
            .execute(valid_cmd().into_command())
            .expect_err("emitting more than the remaining balance must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
        assert_eq!(pool.remaining_balance(), 500);
    }

    // Scenario: rejected - The low-pool warning must be raised at 80% depletion
    // (advance notice), not at exhaustion.
    #[test]
    fn rejects_when_low_pool_warning_is_misconfigured() {
        let mut pool = ready_pool();
        pool.set_low_pool_warning_valid(false);

        let err = pool
            .execute(valid_cmd().into_command())
            .expect_err("a misconfigured low-pool warning must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
    }

    // Scenario: rejected - Pool size is a governance-adjustable parameter; a
    // later season cannot open until the prior season's drain is understood.
    #[test]
    fn rejects_when_governance_gate_is_unresolved() {
        let mut pool = ready_pool();
        pool.set_governance_schedule_valid(false);

        let err = pool
            .execute(valid_cmd().into_command())
            .expect_err("an unresolved governance gate must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
    }

    // An emission draining exactly the remaining balance is allowed (the ceiling
    // is inclusive).
    #[test]
    fn emits_exactly_the_remaining_balance() {
        let mut pool = ready_pool();
        pool.set_remaining_balance(1000);

        let events = pool
            .execute(valid_cmd().into_command())
            .expect("emitting exactly the remaining balance should succeed");
        assert_eq!(events.len(), 1);
        assert_eq!(pool.remaining_balance(), 0);
    }

    // Scenario: Successfully execute RaiseLowPoolWarningCmd.
    #[test]
    fn raises_and_emits_low_pool_warning_raised_event() {
        let mut pool = ready_pool();

        let events = pool
            .execute(valid_warning_cmd().into_command())
            .expect("valid low-pool warning should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "low.pool.warning.raised");
        match &events[0] {
            Event::LowPoolWarningRaised(raised) => {
                assert_eq!(raised.pool_id, "pool-01");
                assert_eq!(raised.depletion_pct, WARNING_DEPLETION_PCT);
            }
            other => panic!("expected LowPoolWarningRaised, got {other:?}"),
        }
        // The EmissionPool recorded the event and advanced its version.
        assert_eq!(pool.version(), 1);
        assert_eq!(pool.uncommitted_events().len(), 1);
        assert_eq!(
            pool.uncommitted_events()[0].event_type(),
            "low.pool.warning.raised"
        );
    }

    // Scenario: RaiseLowPoolWarningCmd rejected - Season 1 opens with a 30M pool;
    // each subsequent season's pool halves by 5% of the prior schedule.
    #[test]
    fn warning_rejected_when_schedule_departs_from_curve() {
        let mut pool = ready_pool();
        pool.set_emission_schedule_valid(false);

        let err = pool
            .execute(valid_warning_cmd().into_command())
            .expect_err("a schedule departing from the mandated curve must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
    }

    // Scenario: RaiseLowPoolWarningCmd rejected - The pool can never emit more
    // than its remaining balance.
    #[test]
    fn warning_rejected_when_pool_is_insolvent() {
        let mut pool = ready_pool();
        pool.set_remaining_balance(0);

        let err = pool
            .execute(valid_warning_cmd().into_command())
            .expect_err("a warning on a pool that would over-emit must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
    }

    // Scenario: RaiseLowPoolWarningCmd rejected - The 50% low-pool warning must
    // be raised at 80% depletion (advance notice), not at exhaustion.
    #[test]
    fn warning_rejected_when_warning_threshold_is_incorrect() {
        let mut pool = ready_pool();
        pool.set_low_pool_warning_valid(false);

        let err = pool
            .execute(valid_warning_cmd().into_command())
            .expect_err("a warning wired to fire at exhaustion must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
    }

    // Scenario: RaiseLowPoolWarningCmd rejected - Pool size is a
    // governance-adjustable parameter; Season 2 cannot open until Season 1 drain
    // is understood.
    #[test]
    fn warning_rejected_when_prior_season_drain_not_understood() {
        let mut pool = ready_pool();
        pool.set_governance_schedule_valid(false);

        let err = pool.execute(valid_warning_cmd().into_command()).expect_err(
            "opening the next season before the prior drain is understood must be rejected",
        );
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
    }

    // An unrecognized command is rejected as UnknownCommand naming this aggregate.
    #[test]
    fn rejects_unknown_command() {
        let mut pool = ready_pool();

        let err = pool
            .execute(Command::new("NoSuchCommand"))
            .expect_err("unknown command must be rejected");
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "EmissionPool");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
        assert_eq!(pool.version(), 0);
    }

    // A command naming a different pool is rejected before any invariant runs.
    #[test]
    fn rejects_command_for_a_different_pool() {
        let mut pool = ready_pool();
        let cmd = EmitRewardCmd::new("pool-99", "recipient-7", 1000);

        let err = pool
            .execute(cmd.into_command())
            .expect_err("a command for another pool must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
    }

    // A warning command naming a different pool is rejected before any invariant
    // runs.
    #[test]
    fn warning_rejected_for_a_different_pool() {
        let mut pool = ready_pool();
        let cmd = RaiseLowPoolWarningCmd::new("pool-99", WARNING_DEPLETION_PCT);

        let err = pool
            .execute(cmd.into_command())
            .expect_err("a warning command for another pool must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
    }

    // Commands missing any required field are rejected.
    #[test]
    fn rejects_command_with_missing_fields() {
        for cmd in [
            EmitRewardCmd::new("   ", "recipient-7", 1000),
            EmitRewardCmd::new("pool-01", "   ", 1000),
            EmitRewardCmd::new("pool-01", "recipient-7", 0),
        ] {
            let mut pool = ready_pool();
            let err = pool
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(pool.version(), 0);
        }
    }

    // Warning commands missing the poolId or carrying an out-of-range
    // depletionPct are rejected.
    #[test]
    fn warning_rejected_with_invalid_fields() {
        for cmd in [
            RaiseLowPoolWarningCmd::new("   ", WARNING_DEPLETION_PCT),
            RaiseLowPoolWarningCmd::new("pool-01", 101),
        ] {
            let mut pool = ready_pool();
            let err = pool
                .execute(cmd.into_command())
                .expect_err("an invalid warning command must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(pool.version(), 0);
        }
    }

    // A malformed payload for a recognized command is a domain error, not a panic.
    #[test]
    fn rejects_malformed_emit_reward_payload() {
        let mut pool = ready_pool();

        let err = pool
            .execute(Command::with_payload(EMIT_REWARD, b"not json".to_vec()))
            .expect_err("malformed payload must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
    }

    // A malformed warning payload is a domain error, not a panic.
    #[test]
    fn rejects_malformed_warning_payload() {
        let mut pool = ready_pool();

        let err = pool
            .execute(Command::with_payload(
                RAISE_LOW_POOL_WARNING,
                b"not json".to_vec(),
            ))
            .expect_err("malformed payload must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(pool.version(), 0);
    }

    #[test]
    fn emit_reward_command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, EmitRewardCmd::COMMAND);
        let decoded: EmitRewardCmd = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }

    #[test]
    fn warning_command_payload_round_trips() {
        let cmd = valid_warning_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, RaiseLowPoolWarningCmd::COMMAND);
        let decoded: RaiseLowPoolWarningCmd = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_warning_cmd());
    }
}
