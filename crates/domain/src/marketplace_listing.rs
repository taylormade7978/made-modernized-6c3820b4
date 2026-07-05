//! MarketplaceListing bounded context - peer-to-peer $MADE listings of owned
//! card tokens in the token-and-marketplace context.
//!
//! A [`MarketplaceListing`] lists a single owned token for sale in $MADE. Four
//! invariants govern whether a listing may be created:
//!
//! 1. **Seller ownership** - the seller must own the listed token at listing and
//!    at settlement; a seller cannot list a token they do not hold.
//! 2. **Canonical fee split** - every settled trade applies the 5% fee split:
//!    2.5% treasury / 1.5% reward pool / 1% burn. A listing whose fee schedule
//!    does not encode exactly that split is rejected.
//! 3. **Atomic settlement** - a trade settles atomically: token transfer and fee
//!    split succeed or fail together, never partially.
//! 4. **Jurisdiction gating** - listings and purchases are blocked for
//!    geo-restricted jurisdictions.
//!
//! [`CreateListingCmd`] (`CreateListingCmd`) validates the seller, token, price,
//! and jurisdiction, enforces every invariant, and on success emits
//! [`Event::ListingCreated`] (`listing.created`). [`CancelListingCmd`]
//! (`CancelListingCmd`) withdraws an open listing: it validates the listingId,
//! enforces the same four invariants, and on success emits
//! [`Event::ListingCancelled`] (`listing.cancelled`). [`PurchaseListingCmd`]
//! (`PurchaseListingCmd`) buys a listed token, geo-checked by jurisdiction: it
//! validates the listingId, buyerId, and jurisdiction, enforces the same four
//! invariants at settlement, and on success emits [`Event::ListingPurchased`]
//! (`listing.purchased`). This module is hand-written (it does not use
//! `shared::stub_aggregate!`) but preserves the same public surface as the
//! scaffolded contexts: a [`MarketplaceListing`] aggregate and a
//! [`MarketplaceListingRepository`] port.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "MarketplaceListing";

/// The command name [`MarketplaceListing::execute`] recognizes to list an owned
/// token for sale in $MADE.
const CREATE_LISTING: &str = "CreateListingCmd";

/// The command name [`MarketplaceListing::execute`] recognizes to withdraw an
/// open listing from $MADE.
const CANCEL_LISTING: &str = "CancelListingCmd";

/// The command name [`MarketplaceListing::execute`] recognizes to purchase a
/// listed token in $MADE.
const PURCHASE_LISTING: &str = "PurchaseListingCmd";

/// The mandated marketplace fee split, in basis points (1 bp = 0.01%). Every
/// settled trade applies a 5% fee split of 2.5% treasury / 1.5% reward pool /
/// 1% burn, i.e. 250 / 150 / 100 bps totalling 500 bps.
const TREASURY_BPS: u32 = 250;
const REWARD_POOL_BPS: u32 = 150;
const BURN_BPS: u32 = 100;

/// The fee schedule a settled trade applies. Field names use the token
/// marketplace's `camelCase` schema.
///
/// The canonical split is 2.5% treasury / 1.5% reward pool / 1% burn (a 5%
/// total). A schedule is only [`FeeSchedule::is_canonical`] when it encodes
/// exactly those basis points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeSchedule {
    /// Basis points routed to the treasury.
    pub treasury_bps: u32,
    /// Basis points routed to the reward pool.
    pub reward_pool_bps: u32,
    /// Basis points burned.
    pub burn_bps: u32,
}

impl FeeSchedule {
    /// The canonical 5% split: 2.5% treasury / 1.5% reward pool / 1% burn.
    pub const fn canonical() -> Self {
        Self {
            treasury_bps: TREASURY_BPS,
            reward_pool_bps: REWARD_POOL_BPS,
            burn_bps: BURN_BPS,
        }
    }

    /// Build a fee schedule from explicit basis points.
    pub fn new(treasury_bps: u32, reward_pool_bps: u32, burn_bps: u32) -> Self {
        Self {
            treasury_bps,
            reward_pool_bps,
            burn_bps,
        }
    }

    /// Whether this schedule encodes exactly the mandated 5% fee split.
    fn is_canonical(&self) -> bool {
        *self == Self::canonical()
    }
}

impl Default for FeeSchedule {
    fn default() -> Self {
        Self::canonical()
    }
}

/// The `CreateListingCmd` payload: who is listing, which token, the ask price in
/// $MADE, and the jurisdiction the listing originates from. Field names use the
/// token marketplace's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`CreateListingCmd::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`MarketplaceListing::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateListingCmd {
    /// The seller listing the token; must own the token being listed.
    pub seller_id: String,
    /// The token being listed for sale.
    pub token_id: String,
    /// The ask price, denominated in $MADE base units; must be positive.
    pub price: u64,
    /// The jurisdiction the listing originates from; must not be geo-restricted.
    pub jurisdiction: String,
}

impl CreateListingCmd {
    /// The command name this maps to.
    pub const COMMAND: &'static str = CREATE_LISTING;

    /// Build a command listing `token_id` from `seller_id` at `price` in
    /// `jurisdiction`.
    pub fn new(
        seller_id: impl Into<String>,
        token_id: impl Into<String>,
        price: u64,
        jurisdiction: impl Into<String>,
    ) -> Self {
        Self {
            seller_id: seller_id.into(),
            token_id: token_id.into(),
            price,
            jurisdiction: jurisdiction.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`MarketplaceListing::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("CreateListingCmd is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `CancelListingCmd` payload: which listing to withdraw and the
/// jurisdiction the cancellation originates from. Field names use the token
/// marketplace's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`CancelListingCmd::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`MarketplaceListing::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelListingCmd {
    /// The listing being withdrawn; must be a valid, non-empty identifier.
    pub listing_id: String,
    /// The jurisdiction the cancellation originates from; must not be
    /// geo-restricted.
    pub jurisdiction: String,
}

impl CancelListingCmd {
    /// The command name this maps to.
    pub const COMMAND: &'static str = CANCEL_LISTING;

    /// Build a command withdrawing `listing_id` from `jurisdiction`.
    pub fn new(listing_id: impl Into<String>, jurisdiction: impl Into<String>) -> Self {
        Self {
            listing_id: listing_id.into(),
            jurisdiction: jurisdiction.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`MarketplaceListing::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("CancelListingCmd is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The `PurchaseListingCmd` payload: which listing to buy, the buyer making the
/// purchase, and the jurisdiction the purchase is geo-checked against. Field
/// names use the token marketplace's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`PurchaseListingCmd::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`MarketplaceListing::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseListingCmd {
    /// The listing being purchased; must name this MarketplaceListing.
    pub listing_id: String,
    /// The buyer purchasing the listed token; must be a valid identifier.
    pub buyer_id: String,
    /// The jurisdiction the purchase is geo-checked against; must not be
    /// geo-restricted.
    pub jurisdiction: String,
}

impl PurchaseListingCmd {
    /// The command name this maps to.
    pub const COMMAND: &'static str = PURCHASE_LISTING;

    /// Build a command purchasing `listing_id` for `buyer_id`, geo-checked
    /// against `jurisdiction`.
    pub fn new(
        listing_id: impl Into<String>,
        buyer_id: impl Into<String>,
        jurisdiction: impl Into<String>,
    ) -> Self {
        Self {
            listing_id: listing_id.into(),
            buyer_id: buyer_id.into(),
            jurisdiction: jurisdiction.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`MarketplaceListing::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("PurchaseListingCmd is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The listing that was created, carried by [`Event::ListingCreated`] and thus
/// by the emitted `listing.created` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingCreated {
    /// The seller who created the listing.
    pub seller_id: String,
    /// The token that was listed.
    pub token_id: String,
    /// The ask price, in $MADE base units.
    pub price: u64,
    /// The jurisdiction the listing originates from.
    pub jurisdiction: String,
    /// The fee split that will apply when the trade settles.
    pub fee_schedule: FeeSchedule,
}

/// The listing that was withdrawn, carried by [`Event::ListingCancelled`] and
/// thus by the emitted `listing.cancelled` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingCancelled {
    /// The listing that was withdrawn.
    pub listing_id: String,
    /// The jurisdiction the cancellation originated from.
    pub jurisdiction: String,
}

/// The purchase that settled, carried by [`Event::ListingPurchased`] and thus by
/// the emitted `listing.purchased` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingPurchased {
    /// The listing that was purchased.
    pub listing_id: String,
    /// The buyer that purchased the listed token.
    pub buyer_id: String,
    /// The jurisdiction the purchase was geo-checked against.
    pub jurisdiction: String,
    /// The fee schedule applied to the settled trade.
    pub fee_schedule: FeeSchedule,
}

/// Domain events emitted by [`MarketplaceListing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An owned token was listed for sale in $MADE.
    ListingCreated(ListingCreated),
    /// An open listing was withdrawn from $MADE.
    ListingCancelled(ListingCancelled),
    /// A listed token was purchased and the trade settled in $MADE.
    ListingPurchased(ListingPurchased),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::ListingCreated(_) => "listing.created",
            Event::ListingCancelled(_) => "listing.cancelled",
            Event::ListingPurchased(_) => "listing.purchased",
        }
    }
}

/// The MarketplaceListing aggregate: one owned token listed for sale in $MADE.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so surrounding wiring stays consistent, while it
/// carries the state [`CreateListingCmd`] validates: whether the seller owns the
/// listed token, the fee schedule a settled trade applies, whether settlement is
/// atomic, and which jurisdictions are geo-restricted.
#[derive(Debug)]
pub struct MarketplaceListing {
    id: String,
    root: AggregateRoot,
    /// Whether the seller owns the listed token at listing (and at settlement).
    seller_owns_listed_token: bool,
    /// The fee split a settled trade applies; must be the canonical 5% split.
    fee_schedule: FeeSchedule,
    /// Whether settlement is atomic (token transfer and fee split together).
    settlement_atomic: bool,
    /// Jurisdictions for which listings and purchases are blocked.
    geo_restricted_jurisdictions: Vec<String>,
}

impl MarketplaceListing {
    /// Create a new, listing-ready MarketplaceListing identified by `id`: the
    /// seller owns the listed token, the canonical 5% fee split applies,
    /// settlement is atomic, and no jurisdictions are geo-restricted. Use the
    /// configuration methods to drive it to the state a command validates.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            seller_owns_listed_token: true,
            fee_schedule: FeeSchedule::canonical(),
            settlement_atomic: true,
            geo_restricted_jurisdictions: Vec::new(),
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

    /// Record whether the seller owns the listed token (`false` models a seller
    /// listing a token they do not hold).
    pub fn set_seller_owns_listed_token(&mut self, owns: bool) {
        self.seller_owns_listed_token = owns;
    }

    /// Set the fee schedule a settled trade applies (`non-canonical` models a
    /// listing that would not apply the mandated 5% split).
    pub fn set_fee_schedule(&mut self, fee_schedule: FeeSchedule) {
        self.fee_schedule = fee_schedule;
    }

    /// Record whether settlement is atomic (`false` models a trade whose token
    /// transfer and fee split could succeed or fail independently).
    pub fn set_settlement_atomic(&mut self, atomic: bool) {
        self.settlement_atomic = atomic;
    }

    /// Mark a jurisdiction as geo-restricted, blocking listings and purchases
    /// that originate from it.
    pub fn restrict_jurisdiction(&mut self, jurisdiction: impl Into<String>) {
        self.geo_restricted_jurisdictions.push(jurisdiction.into());
    }

    /// Ownership invariant: the seller must own the listed token at listing and
    /// at settlement.
    fn ensure_seller_owns_listed_token(&self) -> Result<(), DomainError> {
        if !self.seller_owns_listed_token {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' seller does not own the listed token; the seller must \
                 own the listed token at listing and at settlement",
                self.id
            )));
        }
        Ok(())
    }

    /// Fee-split invariant: every settled trade applies the 5% fee split of
    /// 2.5% treasury / 1.5% reward pool / 1% burn.
    fn ensure_canonical_fee_split(&self) -> Result<(), DomainError> {
        if !self.fee_schedule.is_canonical() {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' fee schedule does not apply the mandated 5% split \
                 (2.5% treasury / 1.5% reward pool / 1% burn)",
                self.id
            )));
        }
        Ok(())
    }

    /// Atomicity invariant: a trade settles atomically - token transfer and fee
    /// split succeed or fail together.
    fn ensure_settlement_atomic(&self) -> Result<(), DomainError> {
        if !self.settlement_atomic {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' settlement is not atomic; a trade settles atomically - \
                 token transfer and fee split succeed or fail together",
                self.id
            )));
        }
        Ok(())
    }

    /// Jurisdiction invariant: listings and purchases are blocked for
    /// geo-restricted jurisdictions.
    fn ensure_jurisdiction_allowed(&self, jurisdiction: &str) -> Result<(), DomainError> {
        if self
            .geo_restricted_jurisdictions
            .iter()
            .any(|restricted| restricted == jurisdiction)
        {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' jurisdiction '{jurisdiction}' is geo-restricted; \
                 listings and purchases are blocked for geo-restricted jurisdictions",
                self.id
            )));
        }
        Ok(())
    }

    /// Handle `CreateListingCmd`: verify the command carries a valid sellerId,
    /// tokenId, price, and jurisdiction; enforce every token marketplace
    /// invariant; and emit [`Event::ListingCreated`].
    fn create_listing(&mut self, cmd: CreateListingCmd) -> Result<Vec<Event>, DomainError> {
        if cmd.seller_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' requires a valid sellerId to create a listing",
                self.id
            )));
        }
        if cmd.token_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' requires a valid tokenId to create a listing",
                self.id
            )));
        }
        if cmd.price == 0 {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' requires a positive price to create a listing",
                self.id
            )));
        }
        if cmd.jurisdiction.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' requires a valid jurisdiction to create a listing",
                self.id
            )));
        }

        self.ensure_seller_owns_listed_token()?;
        self.ensure_canonical_fee_split()?;
        self.ensure_settlement_atomic()?;
        self.ensure_jurisdiction_allowed(&cmd.jurisdiction)?;

        let event = Event::ListingCreated(ListingCreated {
            seller_id: cmd.seller_id,
            token_id: cmd.token_id,
            price: cmd.price,
            jurisdiction: cmd.jurisdiction,
            fee_schedule: self.fee_schedule,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `CancelListingCmd`: verify the command carries a valid listingId,
    /// enforce every token marketplace invariant, and emit
    /// [`Event::ListingCancelled`].
    fn cancel_listing(&mut self, cmd: CancelListingCmd) -> Result<Vec<Event>, DomainError> {
        if cmd.listing_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' requires a valid listingId to cancel a listing",
                self.id
            )));
        }
        if cmd.jurisdiction.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' requires a valid jurisdiction to cancel a listing",
                self.id
            )));
        }

        self.ensure_seller_owns_listed_token()?;
        self.ensure_canonical_fee_split()?;
        self.ensure_settlement_atomic()?;
        self.ensure_jurisdiction_allowed(&cmd.jurisdiction)?;

        let event = Event::ListingCancelled(ListingCancelled {
            listing_id: cmd.listing_id,
            jurisdiction: cmd.jurisdiction,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `PurchaseListingCmd`: verify the command carries a valid listingId
    /// (naming this MarketplaceListing), buyerId, and jurisdiction; enforce every
    /// token marketplace invariant at settlement; and emit
    /// [`Event::ListingPurchased`].
    fn purchase_listing(&mut self, cmd: PurchaseListingCmd) -> Result<Vec<Event>, DomainError> {
        if cmd.listing_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' requires a valid listingId to purchase a listing",
                self.id
            )));
        }
        if cmd.buyer_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' requires a valid buyerId to purchase a listing",
                self.id
            )));
        }
        if cmd.jurisdiction.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "marketplace listing '{}' requires a valid jurisdiction to purchase a listing",
                self.id
            )));
        }
        if cmd.listing_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets marketplace listing '{}' but this aggregate is marketplace \
                 listing '{}'",
                cmd.listing_id, self.id
            )));
        }

        self.ensure_seller_owns_listed_token()?;
        self.ensure_canonical_fee_split()?;
        self.ensure_settlement_atomic()?;
        self.ensure_jurisdiction_allowed(&cmd.jurisdiction)?;

        let event = Event::ListingPurchased(ListingPurchased {
            listing_id: cmd.listing_id,
            buyer_id: cmd.buyer_id,
            jurisdiction: cmd.jurisdiction,
            fee_schedule: self.fee_schedule,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for MarketplaceListing {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            CREATE_LISTING => {
                let cmd: CreateListingCmd =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed CreateListingCmd payload: {e}"
                        ))
                    })?;
                self.create_listing(cmd)
            }
            CANCEL_LISTING => {
                let cmd: CancelListingCmd =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed CancelListingCmd payload: {e}"
                        ))
                    })?;
                self.cancel_listing(cmd)
            }
            PURCHASE_LISTING => {
                let cmd: PurchaseListingCmd =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed PurchaseListingCmd payload: {e}"
                        ))
                    })?;
                self.purchase_listing(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`MarketplaceListing`] aggregate. Adapters
/// implement [`shared::Repository`] for `MarketplaceListing` and then this
/// marker trait.
pub trait MarketplaceListingRepository: Repository<MarketplaceListing> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listing-ready MarketplaceListing `listing-01`: the seller owns the
    /// listed token, the canonical 5% fee split applies, settlement is atomic,
    /// and no jurisdictions are geo-restricted.
    fn ready_listing() -> MarketplaceListing {
        let mut listing = MarketplaceListing::new("listing-01");
        listing.set_seller_owns_listed_token(true);
        listing.set_fee_schedule(FeeSchedule::canonical());
        listing.set_settlement_atomic(true);
        listing
    }

    /// A command listing `token-01` from `seller-01` at 1000 $MADE in the `US`
    /// jurisdiction.
    fn valid_cmd() -> CreateListingCmd {
        CreateListingCmd::new("seller-01", "token-01", 1000, "US")
    }

    // Scenario: Successfully execute CreateListingCmd.
    #[test]
    fn creates_and_emits_listing_created_event() {
        let mut listing = ready_listing();

        let events = listing
            .execute(valid_cmd().into_command())
            .expect("valid listing should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "listing.created");
        match &events[0] {
            Event::ListingCreated(created) => {
                assert_eq!(created.seller_id, "seller-01");
                assert_eq!(created.token_id, "token-01");
                assert_eq!(created.price, 1000);
                assert_eq!(created.jurisdiction, "US");
                assert_eq!(created.fee_schedule, FeeSchedule::canonical());
            }
            other => panic!("expected ListingCreated, got {other:?}"),
        }
        // The MarketplaceListing recorded the event and advanced its version.
        assert_eq!(listing.version(), 1);
        assert_eq!(listing.uncommitted_events().len(), 1);
        assert_eq!(
            listing.uncommitted_events()[0].event_type(),
            "listing.created"
        );
    }

    // Scenario: rejected - The seller must own the listed token at listing and
    // at settlement.
    #[test]
    fn rejects_when_seller_does_not_own_listed_token() {
        let mut listing = ready_listing();
        listing.set_seller_owns_listed_token(false);

        let err = listing
            .execute(valid_cmd().into_command())
            .expect_err("a seller who does not own the token must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // Scenario: rejected - Every settled trade applies the 5% fee split: 2.5%
    // treasury / 1.5% reward pool / 1% burn.
    #[test]
    fn rejects_when_fee_split_is_not_canonical() {
        let mut listing = ready_listing();
        // A split that does not encode the mandated 2.5% / 1.5% / 1%.
        listing.set_fee_schedule(FeeSchedule::new(300, 150, 100));

        let err = listing
            .execute(valid_cmd().into_command())
            .expect_err("a non-canonical fee split must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // Scenario: rejected - A trade settles atomically - token transfer and fee
    // split succeed or fail together.
    #[test]
    fn rejects_when_settlement_is_not_atomic() {
        let mut listing = ready_listing();
        listing.set_settlement_atomic(false);

        let err = listing
            .execute(valid_cmd().into_command())
            .expect_err("non-atomic settlement must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // Scenario: rejected - Listings and purchases are blocked for geo-restricted
    // jurisdictions.
    #[test]
    fn rejects_when_jurisdiction_is_geo_restricted() {
        let mut listing = ready_listing();
        listing.restrict_jurisdiction("US");

        let err = listing
            .execute(valid_cmd().into_command())
            .expect_err("a geo-restricted jurisdiction must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // An unrecognized command is rejected as UnknownCommand naming this aggregate.
    #[test]
    fn rejects_unknown_command() {
        let mut listing = ready_listing();

        let err = listing
            .execute(Command::new("NoSuchCommand"))
            .expect_err("unknown command must be rejected");
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "MarketplaceListing");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
        assert_eq!(listing.version(), 0);
    }

    // A malformed payload for a recognized command is a domain error, not a panic.
    #[test]
    fn rejects_malformed_create_listing_payload() {
        let mut listing = ready_listing();

        let err = listing
            .execute(Command::with_payload(CREATE_LISTING, b"not json".to_vec()))
            .expect_err("malformed payload must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    /// A command withdrawing `listing-01` originating from the `US`
    /// jurisdiction.
    fn valid_cancel_cmd() -> CancelListingCmd {
        CancelListingCmd::new("listing-01", "US")
    }

    // Scenario: Successfully execute CancelListingCmd.
    #[test]
    fn cancels_and_emits_listing_cancelled_event() {
        let mut listing = ready_listing();

        let events = listing
            .execute(valid_cancel_cmd().into_command())
            .expect("valid cancellation should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "listing.cancelled");
        match &events[0] {
            Event::ListingCancelled(cancelled) => {
                assert_eq!(cancelled.listing_id, "listing-01");
                assert_eq!(cancelled.jurisdiction, "US");
            }
            other => panic!("expected ListingCancelled, got {other:?}"),
        }
        // The MarketplaceListing recorded the event and advanced its version.
        assert_eq!(listing.version(), 1);
        assert_eq!(listing.uncommitted_events().len(), 1);
        assert_eq!(
            listing.uncommitted_events()[0].event_type(),
            "listing.cancelled"
        );
    }

    // Scenario: rejected - The seller must own the listed token at listing and
    // at settlement.
    #[test]
    fn cancel_rejects_when_seller_does_not_own_listed_token() {
        let mut listing = ready_listing();
        listing.set_seller_owns_listed_token(false);

        let err = listing
            .execute(valid_cancel_cmd().into_command())
            .expect_err("a seller who does not own the token must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // Scenario: rejected - Every settled trade applies the 5% fee split: 2.5%
    // treasury / 1.5% reward pool / 1% burn.
    #[test]
    fn cancel_rejects_when_fee_split_is_not_canonical() {
        let mut listing = ready_listing();
        // A split that does not encode the mandated 2.5% / 1.5% / 1%.
        listing.set_fee_schedule(FeeSchedule::new(300, 150, 100));

        let err = listing
            .execute(valid_cancel_cmd().into_command())
            .expect_err("a non-canonical fee split must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // Scenario: rejected - A trade settles atomically - token transfer and fee
    // split succeed or fail together.
    #[test]
    fn cancel_rejects_when_settlement_is_not_atomic() {
        let mut listing = ready_listing();
        listing.set_settlement_atomic(false);

        let err = listing
            .execute(valid_cancel_cmd().into_command())
            .expect_err("non-atomic settlement must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // Scenario: rejected - Listings and purchases are blocked for geo-restricted
    // jurisdictions.
    #[test]
    fn cancel_rejects_when_jurisdiction_is_geo_restricted() {
        let mut listing = ready_listing();
        listing.restrict_jurisdiction("US");

        let err = listing
            .execute(valid_cancel_cmd().into_command())
            .expect_err("a geo-restricted jurisdiction must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // A malformed payload for CancelListingCmd is a domain error, not a panic.
    #[test]
    fn rejects_malformed_cancel_listing_payload() {
        let mut listing = ready_listing();

        let err = listing
            .execute(Command::with_payload(CANCEL_LISTING, b"not json".to_vec()))
            .expect_err("malformed payload must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    /// A command purchasing listing `listing-01` for buyer `buyer-7`, geo-checked
    /// against jurisdiction `US`.
    fn valid_purchase_cmd() -> PurchaseListingCmd {
        PurchaseListingCmd::new("listing-01", "buyer-7", "US")
    }

    // Scenario: Successfully execute PurchaseListingCmd.
    #[test]
    fn purchases_and_emits_listing_purchased_event() {
        let mut listing = ready_listing();

        let events = listing
            .execute(valid_purchase_cmd().into_command())
            .expect("valid purchase should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "listing.purchased");
        match &events[0] {
            Event::ListingPurchased(purchased) => {
                assert_eq!(purchased.listing_id, "listing-01");
                assert_eq!(purchased.buyer_id, "buyer-7");
                assert_eq!(purchased.jurisdiction, "US");
                assert_eq!(purchased.fee_schedule, FeeSchedule::canonical());
            }
            other => panic!("expected ListingPurchased, got {other:?}"),
        }
        // The MarketplaceListing recorded the event and advanced its version.
        assert_eq!(listing.version(), 1);
        assert_eq!(listing.uncommitted_events().len(), 1);
        assert_eq!(
            listing.uncommitted_events()[0].event_type(),
            "listing.purchased"
        );
    }

    // Scenario: rejected - The seller must own the listed token at listing and
    // at settlement.
    #[test]
    fn purchase_rejects_when_seller_does_not_own_listed_token() {
        let mut listing = ready_listing();
        listing.set_seller_owns_listed_token(false);

        let err = listing
            .execute(valid_purchase_cmd().into_command())
            .expect_err("a seller who does not own the token must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // Scenario: rejected - Every settled trade applies the 5% fee split: 2.5%
    // treasury / 1.5% reward pool / 1% burn.
    #[test]
    fn purchase_rejects_when_fee_split_is_not_canonical() {
        let mut listing = ready_listing();
        // A split that even sums to the correct 500 bps but misallocates the
        // components is still a violation.
        listing.set_fee_schedule(FeeSchedule::new(300, 100, 100));

        let err = listing
            .execute(valid_purchase_cmd().into_command())
            .expect_err("a non-canonical fee split must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // Scenario: rejected - A trade settles atomically - token transfer and fee
    // split succeed or fail together.
    #[test]
    fn purchase_rejects_when_settlement_is_not_atomic() {
        let mut listing = ready_listing();
        listing.set_settlement_atomic(false);

        let err = listing
            .execute(valid_purchase_cmd().into_command())
            .expect_err("non-atomic settlement must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // Scenario: rejected - Listings and purchases are blocked for geo-restricted
    // jurisdictions.
    #[test]
    fn purchase_rejects_when_jurisdiction_is_geo_restricted() {
        let mut listing = ready_listing();
        listing.restrict_jurisdiction("US");

        let err = listing
            .execute(valid_purchase_cmd().into_command())
            .expect_err("a purchase from a geo-restricted jurisdiction must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // A command naming a different listing is rejected before any invariant runs.
    #[test]
    fn purchase_rejects_command_for_a_different_listing() {
        let mut listing = ready_listing();
        let cmd = PurchaseListingCmd::new("listing-99", "buyer-7", "US");

        let err = listing
            .execute(cmd.into_command())
            .expect_err("a command for another listing must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    // Commands missing any required field are rejected.
    #[test]
    fn purchase_rejects_command_with_missing_fields() {
        for cmd in [
            PurchaseListingCmd::new("   ", "buyer-7", "US"),
            PurchaseListingCmd::new("listing-01", "   ", "US"),
            PurchaseListingCmd::new("listing-01", "buyer-7", "   "),
        ] {
            let mut listing = ready_listing();
            let err = listing
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(listing.version(), 0);
        }
    }

    // A malformed payload for PurchaseListingCmd is a domain error, not a panic.
    #[test]
    fn rejects_malformed_purchase_listing_payload() {
        let mut listing = ready_listing();

        let err = listing
            .execute(Command::with_payload(
                PURCHASE_LISTING,
                b"not json".to_vec(),
            ))
            .expect_err("malformed payload must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(listing.version(), 0);
    }

    #[test]
    fn purchase_command_payload_round_trips() {
        let cmd = valid_purchase_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, PurchaseListingCmd::COMMAND);
        let decoded: PurchaseListingCmd = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_purchase_cmd());
    }
}
