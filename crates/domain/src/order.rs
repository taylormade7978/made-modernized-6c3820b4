//! Order bounded context — a purchase in the shop-and-payments context.
//!
//! An [`Order`] is a single storefront purchase whose payment settles through
//! Stripe. Five invariants govern whether a payment may be confirmed and later
//! fulfilled, and every one of them is re-checked when a Stripe webhook reports
//! a payment intent has succeeded or the purchased entitlements are granted:
//!
//! 1. **Fiat via Stripe only** — payment currency is fiat settled via Stripe; an
//!    Order may never settle in the in-game `$MADE` soft currency.
//! 2. **Total equals line items** — the order total must equal the sum of its
//!    line items; a mismatched total cannot be confirmed or fulfilled.
//! 3. **HMAC-verified webhook** — fulfillment occurs only after payment is
//!    confirmed via an HMAC-verified Stripe webhook; an unverified (spoofable)
//!    webhook may not confirm payment or allow fulfillment.
//! 4. **Idempotent per payment intent** — processing is idempotent per Stripe
//!    payment intent; a payment intent already processed may not be confirmed a
//!    second time (no double-fulfillment).
//! 5. **Refund reverses exactly** — a refund reverses exactly the entitlements
//!    the order granted; an Order whose refund/entitlement ledger is out of
//!    balance may not be confirmed or fulfilled.
//!
//! Four commands are implemented. [`CreateOrder`] (`CreateOrderCmd`) opens a fiat
//! order from a cart of SKUs — given a playerId, lineItems, and a currency it
//! enforces every invariant and, on success, emits [`Event::OrderCreated`]
//! (`order.created`). [`ConfirmPayment`] (`ConfirmPaymentCmd`) then marks payment
//! confirmed from a verified Stripe webhook, enforcing every invariant, and on
//! success emits [`Event::PaymentConfirmed`] (`payment.confirmed`).
//! [`FulfillOrder`] (`FulfillOrderCmd`) grants the purchased entitlements after
//! confirmation, enforcing every invariant, and on success emits
//! [`Event::OrderFulfilled`] (`order.fulfilled`). [`RefundOrder`]
//! (`RefundOrderCmd`) reverses the entitlements granted by the order and emits
//! [`Event::OrderRefunded`] (`order.refunded`). All commands re-check the same
//! five invariants against the aggregate's state. This module is hand-written
//! (it does not use
//! `shared::stub_aggregate!`) but preserves the same public surface — an
//! [`Order`] aggregate and an [`OrderRepository`] port — so any persistence
//! adapters compile against it unchanged, exactly like its sibling
//! [`Outfit`](crate::outfit).

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "Order";

/// The command name that opens a new Order from a cart of SKUs.
const CREATE_ORDER: &str = "CreateOrderCmd";

/// The command name that confirms payment for an existing Order.
const CONFIRM_PAYMENT: &str = "ConfirmPaymentCmd";

/// The command name that grants purchased entitlements after payment.
const FULFILL_ORDER: &str = "FulfillOrderCmd";

/// The command name that refunds an existing Order.
const REFUND_ORDER: &str = "RefundOrderCmd";

/// The `CreateOrderCmd` payload: opens a fiat order for `player_id` from a cart
/// of SKUs (`line_items`) settling in `currency`. Field names use the payments
/// service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`CreateOrder::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`Order::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrder {
    /// The Order being opened; must name this Order, and must be non-empty.
    pub order_id: String,
    /// The player the order is opened for; must be non-empty.
    pub player_id: String,
    /// The cart of SKUs the order is composed of; must be non-empty.
    pub line_items: Vec<String>,
    /// The settlement currency; must be non-empty (and, per the invariants, fiat
    /// via Stripe rather than the in-game `$MADE` currency).
    pub currency: String,
}

impl CreateOrder {
    /// The command name this maps to.
    pub const COMMAND: &'static str = CREATE_ORDER;

    /// Build a command opening `order_id` for `player_id` from `line_items`
    /// settling in `currency`.
    pub fn new(
        order_id: impl Into<String>,
        player_id: impl Into<String>,
        line_items: impl IntoIterator<Item = String>,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            order_id: order_id.into(),
            player_id: player_id.into(),
            line_items: line_items.into_iter().collect(),
            currency: currency.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`Order::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("CreateOrder is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `ConfirmPaymentCmd` payload: which Order is being confirmed and the
/// Stripe payment intent reference the confirmation is for. Field names use the
/// payments service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`ConfirmPayment::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`Order::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmPayment {
    /// The Order the payment is confirmed for; must name this Order, and must be
    /// non-empty.
    pub order_id: String,
    /// The Stripe payment intent the confirmation is for; must be non-empty.
    pub payment_intent_ref: String,
}

impl ConfirmPayment {
    /// The command name this maps to.
    pub const COMMAND: &'static str = CONFIRM_PAYMENT;

    /// Build a command confirming `payment_intent_ref` for `order_id`.
    pub fn new(order_id: impl Into<String>, payment_intent_ref: impl Into<String>) -> Self {
        Self {
            order_id: order_id.into(),
            payment_intent_ref: payment_intent_ref.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`Order::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("ConfirmPayment is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `FulfillOrderCmd` payload: which Order is being fulfilled. Field names
/// use the shop-and-payments service's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`FulfillOrder::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`Order::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FulfillOrder {
    /// The Order being fulfilled; must name this Order, and must be non-empty.
    pub order_id: String,
}

impl FulfillOrder {
    /// The command name this maps to.
    pub const COMMAND: &'static str = FULFILL_ORDER;

    /// Build a command fulfilling `order_id`.
    pub fn new(order_id: impl Into<String>) -> Self {
        Self {
            order_id: order_id.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`Order::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("FulfillOrder is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `RefundOrderCmd` payload: which Order is being refunded and why the
/// refund is being requested. Field names use the payments service's `camelCase`
/// schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`RefundOrder::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`Order::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefundOrder {
    /// The Order being refunded; must name this Order, and must be non-empty.
    pub order_id: String,
    /// The reason for the refund; must be non-empty.
    pub reason: String,
}

impl RefundOrder {
    /// The command name this maps to.
    pub const COMMAND: &'static str = REFUND_ORDER;

    /// Build a command refunding `order_id` for `reason`.
    pub fn new(order_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            order_id: order_id.into(),
            reason: reason.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`Order::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("RefundOrder is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The order that was opened, carried by [`Event::OrderCreated`] and thus by the
/// emitted `order.created` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderCreated {
    /// The Order that was opened.
    pub order_id: String,
    /// The player the order was opened for.
    pub player_id: String,
    /// The cart of SKUs the order was opened from.
    pub line_items: Vec<String>,
    /// The settlement currency the order was opened in.
    pub currency: String,
}

/// The payment that was confirmed, carried by [`Event::PaymentConfirmed`] and
/// thus by the emitted `payment.confirmed` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentConfirmed {
    /// The Order whose payment was confirmed.
    pub order_id: String,
    /// The Stripe payment intent that settled the Order.
    pub payment_intent_ref: String,
}

/// The Order that was fulfilled, carried by [`Event::OrderFulfilled`] and thus
/// by the emitted `order.fulfilled` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderFulfilled {
    /// The Order whose entitlements were granted.
    pub order_id: String,
    /// The Stripe payment intent the fulfillment was recorded against; the
    /// idempotency key that guards against double-fulfillment.
    pub payment_intent_id: String,
}

/// The order that was refunded, carried by [`Event::OrderRefunded`] and thus by
/// the emitted `order.refunded` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRefunded {
    /// The Order whose entitlements were reversed.
    pub order_id: String,
    /// The reason supplied for the refund.
    pub reason: String,
}

/// Domain events emitted by [`Order`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A fiat Order was opened from a cart of SKUs.
    OrderCreated(OrderCreated),
    /// Payment for the Order was confirmed from a verified Stripe webhook.
    PaymentConfirmed(PaymentConfirmed),
    /// The Order's purchased entitlements were granted.
    OrderFulfilled(OrderFulfilled),
    /// The Order was refunded and its granted entitlements were reversed.
    OrderRefunded(OrderRefunded),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::OrderCreated(_) => "order.created",
            Event::PaymentConfirmed(_) => "payment.confirmed",
            Event::OrderFulfilled(_) => "order.fulfilled",
            Event::OrderRefunded(_) => "order.refunded",
        }
    }
}

/// The Order aggregate: one storefront purchase settled through Stripe.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so the surrounding wiring is unchanged, while it
/// now carries the state the implemented commands validate against: whether the
/// payment currency is fiat (never `$MADE`), whether the order total equals the
/// sum of its line items, whether payment was confirmed by an HMAC-verified
/// Stripe webhook, whether this payment intent was already confirmed or
/// fulfilled, and whether the refund/entitlement ledger balances.
///
/// A fresh Order from [`Order::new`] is confirmable and fulfillment-ready: it
/// settles in fiat via Stripe, its total matches its line items, its payment is
/// confirmed by an HMAC-verified webhook, its payment intent has not yet been
/// fulfilled, and its refunds reverse exactly the entitlements granted. The
/// configuration methods below drive it to a state a command rejects, exactly as
/// [`Outfit`](crate::outfit) is built up before a command validates it.
#[derive(Debug)]
pub struct Order {
    id: String,
    root: AggregateRoot,
    /// The Stripe payment intent that pays for this Order; the idempotency key
    /// for fulfillment.
    payment_intent_id: String,
    /// Whether the payment currency is fiat settled via Stripe. `false` means it
    /// would settle in the in-game `$MADE` currency, which is never allowed.
    currency_is_fiat_via_stripe: bool,
    /// The order total, in the currency's minor units.
    order_total: i64,
    /// The sum of the Order's line items, in the currency's minor units. Must
    /// equal [`Order::order_total`] for the Order to be legal.
    line_items_total: i64,
    /// Whether the confirming Stripe webhook's signature was HMAC-verified.
    webhook_hmac_verified: bool,
    /// Whether payment has been confirmed by that HMAC-verified webhook.
    payment_confirmed: bool,
    /// Whether this Stripe payment intent has already been processed. Confirming
    /// an already-processed intent would double-fulfill, so it is rejected.
    payment_intent_already_processed: bool,
    /// Whether the Order has already granted entitlements for its payment
    /// intent. Fulfilling again would double-fulfill.
    already_fulfilled: bool,
    /// Whether every refund reverses exactly the entitlements the order granted
    /// (the refund/entitlement ledger balances).
    refund_reverses_exactly: bool,
}

impl Order {
    /// Create a new, confirmable and fulfillment-ready Order identified by `id`.
    /// It settles in fiat via Stripe, its total matches its line items, its
    /// payment is confirmed by an HMAC-verified webhook, its payment intent has
    /// not been fulfilled, and its refunds reverse exactly the entitlements
    /// granted. Use the configuration methods to drive it to the state a command
    /// validates.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        let payment_intent_id = format!("pi_{id}");
        Self {
            id,
            root: AggregateRoot::new(),
            payment_intent_id,
            currency_is_fiat_via_stripe: true,
            order_total: 0,
            line_items_total: 0,
            webhook_hmac_verified: true,
            payment_confirmed: true,
            payment_intent_already_processed: false,
            already_fulfilled: false,
            refund_reverses_exactly: true,
        }
    }

    /// This aggregate's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The Stripe payment intent that pays for this Order.
    pub fn payment_intent_id(&self) -> &str {
        &self.payment_intent_id
    }

    /// The order total, in the currency's minor units.
    pub fn order_total(&self) -> i64 {
        self.order_total
    }

    /// The sum of the Order's line items, in the currency's minor units.
    pub fn line_items_total(&self) -> i64 {
        self.line_items_total
    }

    /// Current version (delegates to the embedded [`AggregateRoot`]).
    pub fn version(&self) -> u64 {
        self.root.version()
    }

    /// Events produced but not yet persisted.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        self.root.uncommitted_events()
    }

    /// Record whether the payment currency is fiat settled via Stripe (`false`
    /// models an attempt to settle in `$MADE`).
    pub fn set_currency_is_fiat_via_stripe(&mut self, ok: bool) {
        self.currency_is_fiat_via_stripe = ok;
    }

    /// Record whether the order settles in fiat via Stripe (`true`) rather than
    /// in the in-game `$MADE` token (`false`).
    pub fn set_currency_is_fiat(&mut self, ok: bool) {
        self.set_currency_is_fiat_via_stripe(ok);
    }

    /// Record whether the order total equals the sum of its line items.
    pub fn set_total_matches_line_items(&mut self, ok: bool) {
        if ok {
            self.line_items_total = self.order_total;
        } else {
            self.line_items_total = self.order_total.saturating_add(1);
        }
    }

    /// Set the order total, in the currency's minor units.
    pub fn set_order_total(&mut self, total: i64) {
        self.order_total = total;
    }

    /// Set the sum of the Order's line items, in the currency's minor units.
    pub fn set_line_items_total(&mut self, total: i64) {
        self.line_items_total = total;
    }

    /// Record whether the confirming Stripe webhook was HMAC-verified.
    pub fn set_webhook_hmac_verified(&mut self, ok: bool) {
        self.webhook_hmac_verified = ok;
    }

    /// Set the Stripe payment intent that pays for this Order.
    pub fn set_payment_intent_id(&mut self, payment_intent_id: impl Into<String>) {
        self.payment_intent_id = payment_intent_id.into();
    }

    /// Record whether payment has been confirmed via an HMAC-verified Stripe
    /// webhook.
    pub fn set_payment_confirmed(&mut self, confirmed: bool) {
        self.payment_confirmed = confirmed;
    }

    /// Record whether this Stripe payment intent has already been processed.
    pub fn set_payment_intent_already_processed(&mut self, already: bool) {
        self.payment_intent_already_processed = already;
        self.already_fulfilled = already;
    }

    /// Record whether the Order has already been fulfilled for its payment
    /// intent.
    pub fn set_already_fulfilled(&mut self, fulfilled: bool) {
        self.already_fulfilled = fulfilled;
    }

    /// Record whether every refund reverses exactly the entitlements granted.
    pub fn set_refund_reverses_exactly(&mut self, ok: bool) {
        self.refund_reverses_exactly = ok;
    }

    /// Currency invariant: payment currency is fiat via Stripe only — an Order
    /// may never settle in `$MADE`.
    fn ensure_fiat_via_stripe(&self) -> Result<(), DomainError> {
        if !self.currency_is_fiat_via_stripe {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' would settle in $MADE; payment currency is fiat via Stripe only — an \
                 Order may never settle in $MADE",
                self.id
            )));
        }
        Ok(())
    }

    /// Total invariant: the order total must equal the sum of its line items.
    fn ensure_total_matches_line_items(&self) -> Result<(), DomainError> {
        if self.order_total != self.line_items_total {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' total {} does not equal its line-item sum {}; the order total must \
                 equal the sum of its line items",
                self.id, self.order_total, self.line_items_total
            )));
        }
        Ok(())
    }

    /// Webhook invariant: fulfillment occurs only after payment is confirmed via
    /// an HMAC-verified Stripe webhook.
    fn ensure_webhook_hmac_verified(&self) -> Result<(), DomainError> {
        if !self.webhook_hmac_verified {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' payment was not confirmed via an HMAC-verified Stripe webhook; \
                 fulfillment occurs only after payment is confirmed via an HMAC-verified Stripe \
                 webhook",
                self.id
            )));
        }
        Ok(())
    }

    /// Confirmation invariant: fulfillment occurs only after payment is
    /// confirmed via an HMAC-verified Stripe webhook.
    fn ensure_payment_confirmed_via_webhook(&self) -> Result<(), DomainError> {
        self.ensure_webhook_hmac_verified()?;
        if !self.payment_confirmed {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' has no confirmed payment; fulfillment occurs only after payment is \
                 confirmed via an HMAC-verified Stripe webhook",
                self.id
            )));
        }
        Ok(())
    }

    /// Idempotency invariant: processing is idempotent per Stripe payment intent
    /// — an already-processed intent must not be confirmed again (no
    /// double-fulfillment).
    fn ensure_not_already_processed(&self) -> Result<(), DomainError> {
        if self.payment_intent_already_processed {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' payment intent was already processed; processing is idempotent per \
                 Stripe payment intent (no double-fulfillment)",
                self.id
            )));
        }
        Ok(())
    }

    /// Fulfillment idempotency invariant: an Order already fulfilled for its
    /// payment intent may not be fulfilled again.
    fn ensure_not_already_fulfilled(&self) -> Result<(), DomainError> {
        if self.already_fulfilled {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' is already fulfilled for payment intent '{}'; processing is \
                 idempotent per Stripe payment intent (no double-fulfillment)",
                self.id, self.payment_intent_id
            )));
        }
        Ok(())
    }

    /// Refund idempotency invariant: a processed intent is valid for refund only
    /// when this Order's own successful confirmation produced that processed
    /// state. Otherwise refunding would operate on an externally pre-processed
    /// intent and risk double-fulfillment.
    fn ensure_refund_intent_state_is_consistent(&self) -> Result<(), DomainError> {
        if self.payment_intent_already_processed && !self.payment_confirmed {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' payment intent was already processed outside this Order; processing \
                 is idempotent per Stripe payment intent (no double-fulfillment)",
                self.id
            )));
        }
        Ok(())
    }

    /// Refund invariant: a refund reverses exactly the entitlements the order
    /// granted.
    fn ensure_refund_reverses_exactly(&self) -> Result<(), DomainError> {
        if !self.refund_reverses_exactly {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' refund/entitlement ledger is out of balance; a refund reverses exactly \
                 the entitlements the order granted",
                self.id
            )));
        }
        Ok(())
    }

    /// Handle `CreateOrderCmd`: verify the command carries a valid orderId
    /// (naming this Order), a playerId, a non-empty cart of line items, and a
    /// currency; enforce every invariant (fiat via Stripe, total-equals-line-
    /// items, HMAC-verified webhook, idempotency, and refund-reverses-exactly);
    /// and emit [`Event::OrderCreated`].
    fn create_order(&mut self, cmd: CreateOrder) -> Result<Vec<Event>, DomainError> {
        // A valid orderId, playerId, line items, and currency must be supplied.
        if cmd.order_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' requires a valid orderId to be created",
                self.id
            )));
        }
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' requires a valid playerId to be created",
                self.id
            )));
        }
        if cmd.line_items.is_empty() || cmd.line_items.iter().any(|sku| sku.trim().is_empty()) {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' requires a non-empty cart of lineItems to be created",
                self.id
            )));
        }
        if cmd.currency.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' requires a valid currency to be created",
                self.id
            )));
        }
        // The command must name the Order it is dispatched to.
        if cmd.order_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets order '{}' but this aggregate is order '{}'",
                cmd.order_id, self.id
            )));
        }

        // Enforce every invariant before recording the creation.
        self.ensure_fiat_via_stripe()?;
        self.ensure_total_matches_line_items()?;
        self.ensure_webhook_hmac_verified()?;
        self.ensure_not_already_processed()?;
        self.ensure_refund_reverses_exactly()?;

        let event = Event::OrderCreated(OrderCreated {
            order_id: cmd.order_id,
            player_id: cmd.player_id,
            line_items: cmd.line_items,
            currency: cmd.currency,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `ConfirmPaymentCmd`: verify the command carries a valid orderId
    /// (naming this Order) and paymentIntentRef, enforce every invariant (fiat
    /// via Stripe, total-equals-line-items, HMAC-verified webhook, idempotency,
    /// and refund-reverses-exactly), mark the payment intent processed, and emit
    /// [`Event::PaymentConfirmed`].
    fn confirm_payment(&mut self, cmd: ConfirmPayment) -> Result<Vec<Event>, DomainError> {
        // A valid orderId and paymentIntentRef must be supplied.
        if cmd.order_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' requires a valid orderId to confirm payment",
                self.id
            )));
        }
        if cmd.payment_intent_ref.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' requires a valid paymentIntentRef to confirm payment",
                self.id
            )));
        }
        // The command must name the Order it is dispatched to.
        if cmd.order_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets order '{}' but this aggregate is order '{}'",
                cmd.order_id, self.id
            )));
        }

        // Enforce every invariant before recording the confirmation.
        self.ensure_fiat_via_stripe()?;
        self.ensure_total_matches_line_items()?;
        self.ensure_webhook_hmac_verified()?;
        self.ensure_not_already_processed()?;
        self.ensure_refund_reverses_exactly()?;

        // Mark the payment intent processed so a replayed webhook for the same
        // intent is rejected by the idempotency invariant — no double-fulfillment.
        self.payment_intent_id = cmd.payment_intent_ref.clone();
        self.payment_confirmed = true;
        self.payment_intent_already_processed = true;

        let event = Event::PaymentConfirmed(PaymentConfirmed {
            order_id: cmd.order_id,
            payment_intent_ref: cmd.payment_intent_ref,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `FulfillOrderCmd`: verify the command carries a valid orderId
    /// (naming this Order), enforce every invariant (fiat via Stripe, total-
    /// equals-line-items, HMAC-verified confirmed payment, idempotency, and
    /// refund-reverses-exactly), grant the purchased entitlements, and emit
    /// [`Event::OrderFulfilled`].
    fn fulfill_order(&mut self, cmd: FulfillOrder) -> Result<Vec<Event>, DomainError> {
        // A valid orderId must be supplied.
        if cmd.order_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' requires a valid orderId to fulfill",
                self.id
            )));
        }
        // The command must name the Order it is dispatched to.
        if cmd.order_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets order '{}' but this aggregate is order '{}'",
                cmd.order_id, self.id
            )));
        }

        // Enforce every invariant before granting entitlements.
        self.ensure_fiat_via_stripe()?;
        self.ensure_total_matches_line_items()?;
        self.ensure_payment_confirmed_via_webhook()?;
        self.ensure_not_already_fulfilled()?;
        self.ensure_refund_reverses_exactly()?;

        // Fulfillment is idempotent per Stripe payment intent: once granted, a
        // second FulfillOrderCmd for the same intent is rejected.
        self.already_fulfilled = true;

        let event = Event::OrderFulfilled(OrderFulfilled {
            order_id: cmd.order_id,
            payment_intent_id: self.payment_intent_id.clone(),
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `RefundOrderCmd`: verify the command carries a valid orderId
    /// (naming this Order) and refund reason; enforce every invariant (fiat via
    /// Stripe, total-equals-line-items, HMAC-verified webhook, idempotency, and
    /// refund-reverses-exactly); and emit [`Event::OrderRefunded`] to represent
    /// the entitlement reversal.
    fn refund_order(&mut self, cmd: RefundOrder) -> Result<Vec<Event>, DomainError> {
        // A valid orderId and reason must be supplied.
        if cmd.order_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' requires a valid orderId to refund",
                self.id
            )));
        }
        if cmd.reason.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "order '{}' requires a valid reason to refund",
                self.id
            )));
        }
        // The command must name the Order it is dispatched to.
        if cmd.order_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets order '{}' but this aggregate is order '{}'",
                cmd.order_id, self.id
            )));
        }

        // Enforce every invariant before recording the refund.
        self.ensure_fiat_via_stripe()?;
        self.ensure_total_matches_line_items()?;
        self.ensure_webhook_hmac_verified()?;
        self.ensure_refund_intent_state_is_consistent()?;
        self.ensure_refund_reverses_exactly()?;

        let event = Event::OrderRefunded(OrderRefunded {
            order_id: cmd.order_id,
            reason: cmd.reason,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for Order {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            CREATE_ORDER => {
                let cmd: CreateOrder = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!(
                        "malformed CreateOrderCmd payload: {e}"
                    ))
                })?;
                self.create_order(cmd)
            }
            CONFIRM_PAYMENT => {
                let cmd: ConfirmPayment =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed ConfirmPaymentCmd payload: {e}"
                        ))
                    })?;
                self.confirm_payment(cmd)
            }
            FULFILL_ORDER => {
                let cmd: FulfillOrder = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!(
                        "malformed FulfillOrderCmd payload: {e}"
                    ))
                })?;
                self.fulfill_order(cmd)
            }
            REFUND_ORDER => {
                let cmd: RefundOrder = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!(
                        "malformed RefundOrderCmd payload: {e}"
                    ))
                })?;
                self.refund_order(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`Order`] aggregate. Adapters implement
/// [`shared::Repository`] for `Order` and then this marker trait.
pub trait OrderRepository: Repository<Order> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A confirmable and fulfillment-ready Order `o-01`: fiat via Stripe, total
    /// matches line items, HMAC-verified webhook, payment confirmed, payment
    /// intent not yet processed or fulfilled, refunds reverse exactly. Tests
    /// mutate one aspect at a time to drive a specific rejection.
    fn ready_order() -> Order {
        let mut order = Order::new("o-01");
        order.set_currency_is_fiat_via_stripe(true);
        order.set_order_total(4200);
        order.set_line_items_total(4200);
        order.set_total_matches_line_items(true);
        order.set_webhook_hmac_verified(true);
        order.set_payment_confirmed(true);
        order.set_payment_intent_already_processed(false);
        order.set_already_fulfilled(false);
        order.set_refund_reverses_exactly(true);
        order
    }

    /// A command confirming payment intent `pi_123` for order `o-01`.
    fn valid_cmd() -> ConfirmPayment {
        ConfirmPayment::new("o-01", "pi_123")
    }

    /// A command refunding order `o-01` because the buyer requested it.
    fn valid_refund_cmd() -> RefundOrder {
        RefundOrder::new("o-01", "buyer requested refund")
    }

    /// A command opening order `o-01` for player `p-01` from a two-SKU cart
    /// settling in USD.
    fn valid_create_cmd() -> CreateOrder {
        CreateOrder::new(
            "o-01",
            "p-01",
            ["sku-hoodie".to_string(), "sku-cap".to_string()],
            "USD",
        )
    }

    /// A command fulfilling order `o-01`.
    fn valid_fulfill_cmd() -> FulfillOrder {
        FulfillOrder::new("o-01")
    }

    // Scenario: Successfully execute CreateOrderCmd.
    #[test]
    fn creates_and_emits_order_created_event() {
        let mut order = ready_order();

        let events = order
            .execute(valid_create_cmd().into_command())
            .expect("valid creation should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "order.created");
        match &events[0] {
            Event::OrderCreated(created) => {
                assert_eq!(created.order_id, "o-01");
                assert_eq!(created.player_id, "p-01");
                assert_eq!(created.line_items, vec!["sku-hoodie", "sku-cap"]);
                assert_eq!(created.currency, "USD");
            }
            other => panic!("expected OrderCreated, got {other:?}"),
        }
        // The Order recorded the event.
        assert_eq!(order.version(), 1);
        assert_eq!(order.uncommitted_events().len(), 1);
        assert_eq!(order.uncommitted_events()[0].event_type(), "order.created");
    }

    // Scenario: rejected — Payment currency is fiat via Stripe only — an Order
    // may never settle in $MADE.
    #[test]
    fn create_rejects_when_currency_is_not_fiat_via_stripe() {
        let mut order = ready_order();
        // The Order attempts to settle in $MADE rather than fiat via Stripe.
        order.set_currency_is_fiat_via_stripe(false);

        let err = order
            .execute(valid_create_cmd().into_command())
            .expect_err("an Order settling in $MADE must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: rejected — The order total must equal the sum of its line items.
    #[test]
    fn create_rejects_when_total_does_not_match_line_items() {
        let mut order = ready_order();
        // The order total no longer equals the sum of its line items.
        order.set_total_matches_line_items(false);

        let err = order
            .execute(valid_create_cmd().into_command())
            .expect_err("an Order whose total mismatches its line items must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: rejected — Fulfillment occurs only after payment is confirmed via
    // an HMAC-verified Stripe webhook.
    #[test]
    fn create_rejects_when_webhook_not_hmac_verified() {
        let mut order = ready_order();
        // The confirming webhook's HMAC signature was not verified.
        order.set_webhook_hmac_verified(false);

        let err = order
            .execute(valid_create_cmd().into_command())
            .expect_err("an unverified Stripe webhook must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: rejected — Processing is idempotent per Stripe payment intent (no
    // double-fulfillment).
    #[test]
    fn create_rejects_when_payment_intent_already_processed() {
        let mut order = ready_order();
        // This payment intent has already been processed once.
        order.set_payment_intent_already_processed(true);

        let err = order
            .execute(valid_create_cmd().into_command())
            .expect_err("an already-processed payment intent must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: rejected — A refund reverses exactly the entitlements the order
    // granted.
    #[test]
    fn create_rejects_when_refund_does_not_reverse_exactly() {
        let mut order = ready_order();
        // The refund/entitlement ledger is out of balance.
        order.set_refund_reverses_exactly(false);

        let err = order
            .execute(valid_create_cmd().into_command())
            .expect_err("an out-of-balance refund ledger must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // A CreateOrderCmd naming a different Order is rejected before any invariant
    // runs.
    #[test]
    fn create_rejects_command_for_a_different_order() {
        let mut order = ready_order();
        let cmd = CreateOrder::new("o-99", "p-01", ["sku-cap".to_string()], "USD");

        let err = order
            .execute(cmd.into_command())
            .expect_err("a command for another order must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // CreateOrderCmd missing any required field (playerId, lineItems, currency,
    // or an empty SKU) is rejected.
    #[test]
    fn create_rejects_command_with_missing_fields() {
        for cmd in [
            CreateOrder::new("o-01", "   ", ["sku-cap".to_string()], "USD"),
            CreateOrder::new("o-01", "p-01", Vec::<String>::new(), "USD"),
            CreateOrder::new("o-01", "p-01", ["   ".to_string()], "USD"),
            CreateOrder::new("o-01", "p-01", ["sku-cap".to_string()], "   "),
        ] {
            let mut order = ready_order();
            let err = order
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(order.version(), 0);
        }
    }

    #[test]
    fn create_command_payload_round_trips() {
        let cmd = valid_create_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, CreateOrder::COMMAND);
        let decoded: CreateOrder = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_create_cmd());
    }

    // Scenario: Successfully execute FulfillOrderCmd.
    #[test]
    fn fulfills_and_emits_order_fulfilled_event() {
        let mut order = ready_order();

        let events = order
            .execute(valid_fulfill_cmd().into_command())
            .expect("valid fulfillment should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "order.fulfilled");
        match &events[0] {
            Event::OrderFulfilled(fulfilled) => {
                assert_eq!(fulfilled.order_id, "o-01");
                assert_eq!(fulfilled.payment_intent_id, "pi_o-01");
            }
            other => panic!("expected OrderFulfilled, got {other:?}"),
        }
        // The Order recorded the event.
        assert_eq!(order.version(), 1);
        assert_eq!(order.uncommitted_events().len(), 1);
        assert_eq!(
            order.uncommitted_events()[0].event_type(),
            "order.fulfilled"
        );
    }

    // Scenario: FulfillOrderCmd rejected — Payment currency is fiat via Stripe
    // only — an Order may never settle in $MADE.
    #[test]
    fn fulfill_rejects_when_currency_is_not_fiat_via_stripe() {
        let mut order = ready_order();
        // The Order attempts to settle in $MADE rather than fiat via Stripe.
        order.set_currency_is_fiat(false);

        let err = order
            .execute(valid_fulfill_cmd().into_command())
            .expect_err("an Order settling in $MADE must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: FulfillOrderCmd rejected — The order total must equal the sum of
    // its line items.
    #[test]
    fn fulfill_rejects_when_total_does_not_match_line_items() {
        let mut order = ready_order();
        // The recorded total no longer equals the sum of the line items.
        order.set_line_items_total(4199);

        let err = order
            .execute(valid_fulfill_cmd().into_command())
            .expect_err("an Order whose total mismatches its line items must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: FulfillOrderCmd rejected — Fulfillment occurs only after payment
    // is confirmed via an HMAC-verified Stripe webhook.
    #[test]
    fn fulfill_rejects_when_payment_not_confirmed() {
        let mut order = ready_order();
        // Payment has not been confirmed by an HMAC-verified Stripe webhook.
        order.set_payment_confirmed(false);

        let err = order
            .execute(valid_fulfill_cmd().into_command())
            .expect_err("an unconfirmed payment must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: FulfillOrderCmd rejected — Fulfillment occurs only after payment
    // is confirmed via an HMAC-verified Stripe webhook.
    #[test]
    fn fulfill_rejects_when_webhook_not_hmac_verified() {
        let mut order = ready_order();
        // The confirming webhook's HMAC signature was not verified.
        order.set_webhook_hmac_verified(false);

        let err = order
            .execute(valid_fulfill_cmd().into_command())
            .expect_err("an unverified Stripe webhook must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: FulfillOrderCmd rejected — Processing is idempotent per Stripe
    // payment intent (no double-fulfillment).
    #[test]
    fn fulfill_rejects_when_already_fulfilled() {
        let mut order = ready_order();
        // The Order was already fulfilled for its payment intent.
        order.set_already_fulfilled(true);

        let err = order
            .execute(valid_fulfill_cmd().into_command())
            .expect_err("a double-fulfillment must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: FulfillOrderCmd rejected — A refund reverses exactly the
    // entitlements the order granted.
    #[test]
    fn fulfill_rejects_when_refund_does_not_reverse_exactly() {
        let mut order = ready_order();
        // The refund/entitlement ledger is out of balance.
        order.set_refund_reverses_exactly(false);

        let err = order
            .execute(valid_fulfill_cmd().into_command())
            .expect_err("an out-of-balance refund ledger must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Fulfillment is idempotent per payment intent: a second FulfillOrderCmd is
    // rejected once the first has granted the entitlements.
    #[test]
    fn second_fulfillment_is_rejected() {
        let mut order = ready_order();

        order
            .execute(valid_fulfill_cmd().into_command())
            .expect("first fulfillment should succeed");

        let err = order
            .execute(valid_fulfill_cmd().into_command())
            .expect_err("a second fulfillment for the same payment intent must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        // No second event was recorded.
        assert_eq!(order.version(), 1);
        assert_eq!(order.uncommitted_events().len(), 1);
    }

    // A confirmed payment intent is the idempotency key carried by fulfillment.
    #[test]
    fn fulfillment_uses_confirmed_payment_intent() {
        let mut order = ready_order();

        order
            .execute(valid_cmd().into_command())
            .expect("payment confirmation should succeed");
        let events = order
            .execute(valid_fulfill_cmd().into_command())
            .expect("fulfillment after confirmation should succeed");

        match &events[0] {
            Event::OrderFulfilled(fulfilled) => {
                assert_eq!(fulfilled.order_id, "o-01");
                assert_eq!(fulfilled.payment_intent_id, "pi_123");
            }
            other => panic!("expected OrderFulfilled, got {other:?}"),
        }
        assert_eq!(order.version(), 2);
        assert_eq!(order.uncommitted_events().len(), 2);
    }

    // A FulfillOrderCmd naming a different Order is rejected before any
    // invariant runs.
    #[test]
    fn fulfill_rejects_command_for_a_different_order() {
        let mut order = ready_order();
        let cmd = FulfillOrder::new("o-99");

        let err = order
            .execute(cmd.into_command())
            .expect_err("a command for another order must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // FulfillOrderCmd missing orderId is rejected.
    #[test]
    fn fulfill_rejects_command_with_missing_order_id() {
        let mut order = ready_order();
        let err = order
            .execute(FulfillOrder::new("   ").into_command())
            .expect_err("a command with a missing orderId must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    #[test]
    fn fulfill_command_payload_round_trips() {
        let cmd = valid_fulfill_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, FulfillOrder::COMMAND);
        let decoded: FulfillOrder = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_fulfill_cmd());
    }

    // Scenario: Successfully execute ConfirmPaymentCmd.
    #[test]
    fn confirms_and_emits_payment_confirmed_event() {
        let mut order = ready_order();

        let events = order
            .execute(valid_cmd().into_command())
            .expect("valid confirmation should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "payment.confirmed");
        match &events[0] {
            Event::PaymentConfirmed(confirmed) => {
                assert_eq!(confirmed.order_id, "o-01");
                assert_eq!(confirmed.payment_intent_ref, "pi_123");
            }
            other => panic!("expected PaymentConfirmed, got {other:?}"),
        }
        // The Order recorded the event.
        assert_eq!(order.version(), 1);
        assert_eq!(order.uncommitted_events().len(), 1);
        assert_eq!(
            order.uncommitted_events()[0].event_type(),
            "payment.confirmed"
        );
    }

    // Scenario: rejected — Payment currency is fiat via Stripe only — an Order
    // may never settle in $MADE.
    #[test]
    fn rejects_when_currency_is_not_fiat_via_stripe() {
        let mut order = ready_order();
        // The Order attempts to settle in $MADE rather than fiat via Stripe.
        order.set_currency_is_fiat_via_stripe(false);

        let err = order
            .execute(valid_cmd().into_command())
            .expect_err("an Order settling in $MADE must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: rejected — The order total must equal the sum of its line items.
    #[test]
    fn rejects_when_total_does_not_match_line_items() {
        let mut order = ready_order();
        // The order total no longer equals the sum of its line items.
        order.set_total_matches_line_items(false);

        let err = order
            .execute(valid_cmd().into_command())
            .expect_err("an Order whose total mismatches its line items must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: rejected — Fulfillment occurs only after payment is confirmed via
    // an HMAC-verified Stripe webhook.
    #[test]
    fn rejects_when_webhook_not_hmac_verified() {
        let mut order = ready_order();
        // The confirming webhook's HMAC signature was not verified.
        order.set_webhook_hmac_verified(false);

        let err = order
            .execute(valid_cmd().into_command())
            .expect_err("an unverified Stripe webhook must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: rejected — Processing is idempotent per Stripe payment intent (no
    // double-fulfillment).
    #[test]
    fn rejects_when_payment_intent_already_processed() {
        let mut order = ready_order();
        // This payment intent has already been processed once.
        order.set_payment_intent_already_processed(true);

        let err = order
            .execute(valid_cmd().into_command())
            .expect_err("an already-processed payment intent must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Idempotency in practice: a second confirmation of the same payment intent
    // is rejected because the first marked it processed (no double-fulfillment).
    #[test]
    fn rejects_a_replayed_confirmation_of_the_same_intent() {
        let mut order = ready_order();

        order
            .execute(valid_cmd().into_command())
            .expect("first confirmation should succeed");
        // The webhook is redelivered for the same intent.
        let err = order
            .execute(valid_cmd().into_command())
            .expect_err("a replayed confirmation must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        // Still exactly one recorded event — no double-fulfillment.
        assert_eq!(order.version(), 1);
        assert_eq!(order.uncommitted_events().len(), 1);
    }

    // Scenario: rejected — A refund reverses exactly the entitlements the order
    // granted.
    #[test]
    fn rejects_when_refund_does_not_reverse_exactly() {
        let mut order = ready_order();
        // The refund/entitlement ledger is out of balance.
        order.set_refund_reverses_exactly(false);

        let err = order
            .execute(valid_cmd().into_command())
            .expect_err("an out-of-balance refund ledger must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // A command naming a different Order is rejected before any invariant runs.
    #[test]
    fn rejects_command_for_a_different_order() {
        let mut order = ready_order();
        let cmd = ConfirmPayment::new("o-99", "pi_123");

        let err = order
            .execute(cmd.into_command())
            .expect_err("a command for another order must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Commands missing any required field are rejected.
    #[test]
    fn rejects_command_with_missing_fields() {
        for cmd in [
            ConfirmPayment::new("   ", "pi_123"),
            ConfirmPayment::new("o-01", "   "),
        ] {
            let mut order = ready_order();
            let err = order
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(order.version(), 0);
        }
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut order = Order::new("o-01");
        let err = order.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "Order");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, ConfirmPayment::COMMAND);
        let decoded: ConfirmPayment = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }

    // Scenario: Successfully execute RefundOrderCmd.
    #[test]
    fn refunds_and_emits_order_refunded_event() {
        let mut order = ready_order();

        let events = order
            .execute(valid_refund_cmd().into_command())
            .expect("valid refund should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "order.refunded");
        match &events[0] {
            Event::OrderRefunded(refunded) => {
                assert_eq!(refunded.order_id, "o-01");
                assert_eq!(refunded.reason, "buyer requested refund");
            }
            other => panic!("expected OrderRefunded, got {other:?}"),
        }
        // The Order recorded the refund event.
        assert_eq!(order.version(), 1);
        assert_eq!(order.uncommitted_events().len(), 1);
        assert_eq!(order.uncommitted_events()[0].event_type(), "order.refunded");
    }

    #[test]
    fn refunds_after_successful_payment_confirmation() {
        let mut order = ready_order();

        order
            .execute(valid_cmd().into_command())
            .expect("payment confirmation should succeed");
        let events = order
            .execute(valid_refund_cmd().into_command())
            .expect("a confirmed order should be refundable");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "order.refunded");
        assert_eq!(order.version(), 2);
        assert_eq!(order.uncommitted_events().len(), 2);
        assert_eq!(
            order.uncommitted_events()[0].event_type(),
            "payment.confirmed"
        );
        assert_eq!(order.uncommitted_events()[1].event_type(), "order.refunded");
    }

    // Scenario: RefundOrderCmd rejected — Payment currency is fiat via Stripe
    // only — an Order may never settle in $MADE.
    #[test]
    fn refund_rejects_when_currency_is_not_fiat_via_stripe() {
        let mut order = ready_order();
        // The Order attempts to settle in $MADE rather than fiat via Stripe.
        order.set_currency_is_fiat_via_stripe(false);

        let err = order
            .execute(valid_refund_cmd().into_command())
            .expect_err("an Order settling in $MADE must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: RefundOrderCmd rejected — The order total must equal the sum of
    // its line items.
    #[test]
    fn refund_rejects_when_total_does_not_match_line_items() {
        let mut order = ready_order();
        // The order total no longer equals the sum of its line items.
        order.set_total_matches_line_items(false);

        let err = order
            .execute(valid_refund_cmd().into_command())
            .expect_err("an Order whose total mismatches its line items must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: RefundOrderCmd rejected — Fulfillment occurs only after payment
    // is confirmed via an HMAC-verified Stripe webhook.
    #[test]
    fn refund_rejects_when_webhook_not_hmac_verified() {
        let mut order = ready_order();
        // The confirming webhook's HMAC signature was not verified.
        order.set_webhook_hmac_verified(false);

        let err = order
            .execute(valid_refund_cmd().into_command())
            .expect_err("an unverified Stripe webhook must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: RefundOrderCmd rejected — Processing is idempotent per Stripe
    // payment intent (no double-fulfillment).
    #[test]
    fn refund_rejects_when_payment_intent_already_processed() {
        let mut order = ready_order();
        // This payment intent was processed outside this Order (processed with no
        // confirmation of our own), so a refund against it is rejected.
        order.set_payment_intent_already_processed(true);
        order.set_payment_confirmed(false);

        let err = order
            .execute(valid_refund_cmd().into_command())
            .expect_err("an already-processed payment intent must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // Scenario: RefundOrderCmd rejected — A refund reverses exactly the
    // entitlements the order granted.
    #[test]
    fn refund_rejects_when_refund_does_not_reverse_exactly() {
        let mut order = ready_order();
        // The refund/entitlement ledger is out of balance.
        order.set_refund_reverses_exactly(false);

        let err = order
            .execute(valid_refund_cmd().into_command())
            .expect_err("an out-of-balance refund ledger must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // RefundOrderCmd naming a different Order is rejected before any invariant
    // runs.
    #[test]
    fn refund_rejects_command_for_a_different_order() {
        let mut order = ready_order();
        let cmd = RefundOrder::new("o-99", "buyer requested refund");

        let err = order
            .execute(cmd.into_command())
            .expect_err("a refund for another order must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(order.version(), 0);
    }

    // RefundOrderCmd missing any required field is rejected.
    #[test]
    fn refund_rejects_command_with_missing_fields() {
        for cmd in [
            RefundOrder::new("   ", "buyer requested refund"),
            RefundOrder::new("o-01", "   "),
        ] {
            let mut order = ready_order();
            let err = order
                .execute(cmd.into_command())
                .expect_err("a refund command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(order.version(), 0);
        }
    }

    #[test]
    fn refund_command_payload_round_trips() {
        let cmd = valid_refund_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, RefundOrder::COMMAND);
        let decoded: RefundOrder = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_refund_cmd());
    }
}
