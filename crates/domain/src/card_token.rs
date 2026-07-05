//! CardToken bounded context - ERC-1155 card tokens with staged IPFS metadata
//! in the token-and-marketplace context.
//!
//! A [`CardToken`] is a single mintable ERC-1155 card token. Four invariants
//! govern whether a token may be minted:
//!
//! 1. **Server-authoritative ownership** - ownership is verified by the server
//!    at match start; client-asserted ownership is never trusted.
//! 2. **Resolvable staged metadata** - every tokenId maps to a resolvable IPFS
//!    metadata record containing name, cost, art URL, and effect ref.
//! 3. **Unique serials** - serialized cosmetic editions carry a unique,
//!    non-reusable serial number.
//! 4. **Verified on-chain render** - a cosmetic renders on-face only after
//!    verified on-chain ownership.
//!
//! [`MintCardTokenCmd`] (`MintCardTokenCmd`) validates the target token,
//! staged metadata reference, and serial number, enforces every invariant, and
//! on success emits [`Event::CardTokenMinted`] (`card.token.minted`).
//!
//! [`StageMetadataCmd`] (`StageMetadataCmd`) pins a card's metadata record to
//! IPFS for a tokenId: it validates the target token, the metadata record
//! (name, cost, art URL, effect ref), and the serialized edition's serial
//! number, enforces the same four invariants, and on success emits
//! [`Event::MetadataStaged`] (`metadata.staged`).
//!
//! [`LinkWalletCmd`] (`LinkWalletCmd`) links a custodial/WalletConnect wallet to
//! a player for a serialized cosmetic edition, geo-checked by jurisdiction: it
//! validates the target token, the player, the wallet address, the jurisdiction,
//! and the serial number, enforces the same four invariants, and on success
//! emits [`Event::WalletLinked`] (`wallet.linked`). This module is hand-written
//! (it does not use `shared::stub_aggregate!`) but preserves the same public
//! surface as the scaffolded contexts: a [`CardToken`] aggregate and a
//! [`CardTokenRepository`] port.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "CardToken";

/// The command name [`CardToken::execute`] recognizes to mint an ERC-1155 card
/// token with staged metadata.
const MINT_CARD_TOKEN: &str = "MintCardTokenCmd";

/// The command name [`CardToken::execute`] recognizes to pin (stage) a card's
/// metadata record to IPFS for a tokenId.
const STAGE_METADATA: &str = "StageMetadataCmd";

/// The command name [`CardToken::execute`] recognizes to link a
/// custodial/WalletConnect wallet to a player for a serialized cosmetic edition.
const LINK_WALLET: &str = "LinkWalletCmd";

/// The `MintCardTokenCmd` payload: which token is being minted, which staged
/// IPFS metadata record it resolves to, and the serialized cosmetic edition's
/// serial number. Field names use the token marketplace's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`MintCardTokenCmd::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`CardToken::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MintCardTokenCmd {
    /// The ERC-1155 token being minted; must name this CardToken.
    pub token_id: String,
    /// IPFS reference for the staged metadata record.
    pub ipfs_metadata_ref: String,
    /// Unique, non-reusable serial number for the cosmetic edition.
    pub serial_number: String,
}

impl MintCardTokenCmd {
    /// The command name this maps to.
    pub const COMMAND: &'static str = MINT_CARD_TOKEN;

    /// Build a command minting `token_id` with staged `ipfs_metadata_ref` and
    /// cosmetic edition `serial_number`.
    pub fn new(
        token_id: impl Into<String>,
        ipfs_metadata_ref: impl Into<String>,
        serial_number: impl Into<String>,
    ) -> Self {
        Self {
            token_id: token_id.into(),
            ipfs_metadata_ref: ipfs_metadata_ref.into(),
            serial_number: serial_number.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`CardToken::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("MintCardTokenCmd is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The token that was minted, carried by [`Event::CardTokenMinted`] and thus by
/// the emitted `card.token.minted` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardTokenMinted {
    /// The ERC-1155 token that was minted.
    pub token_id: String,
    /// IPFS reference for the staged metadata record.
    pub ipfs_metadata_ref: String,
    /// Serialized cosmetic edition serial number.
    pub serial_number: String,
}

/// The IPFS metadata record staged for a tokenId. Every tokenId must map to a
/// resolvable record carrying all four card render and rules fields. Field
/// names use the token marketplace's `camelCase` schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StagedMetadata {
    /// Display name of the card.
    pub name: String,
    /// Play cost of the card.
    pub cost: u32,
    /// URL of the card art asset.
    pub art_url: String,
    /// Reference to the card's effect/rules definition.
    pub effect_ref: String,
}

impl StagedMetadata {
    /// Build a staged metadata record.
    pub fn new(
        name: impl Into<String>,
        cost: u32,
        art_url: impl Into<String>,
        effect_ref: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            cost,
            art_url: art_url.into(),
            effect_ref: effect_ref.into(),
        }
    }

    /// Whether every required field resolves to a non-empty value. `cost` is a
    /// numeric field and may legitimately be zero.
    fn is_resolvable(&self) -> bool {
        !self.name.trim().is_empty()
            && !self.art_url.trim().is_empty()
            && !self.effect_ref.trim().is_empty()
    }
}

/// The `StageMetadataCmd` payload: which token is being staged, the IPFS
/// metadata record it resolves to, and the serialized cosmetic edition's serial
/// number. Field names use the token marketplace's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`StageMetadataCmd::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`CardToken::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageMetadataCmd {
    /// The ERC-1155 token whose metadata is being staged; must name this
    /// CardToken.
    pub token_id: String,
    /// The IPFS metadata record to pin for the token.
    pub metadata: StagedMetadata,
    /// Unique, non-reusable serial number for the cosmetic edition.
    pub serial_number: String,
}

impl StageMetadataCmd {
    /// The command name this maps to.
    pub const COMMAND: &'static str = STAGE_METADATA;

    /// Build a command staging `metadata` for `token_id` under cosmetic edition
    /// `serial_number`.
    pub fn new(
        token_id: impl Into<String>,
        metadata: StagedMetadata,
        serial_number: impl Into<String>,
    ) -> Self {
        Self {
            token_id: token_id.into(),
            metadata,
            serial_number: serial_number.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`CardToken::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("StageMetadataCmd is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The metadata that was staged, carried by [`Event::MetadataStaged`] and thus
/// by the emitted `metadata.staged` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataStaged {
    /// The ERC-1155 token whose metadata was staged.
    pub token_id: String,
    /// The IPFS metadata record pinned for the token.
    pub metadata: StagedMetadata,
    /// Serialized cosmetic edition serial number.
    pub serial_number: String,
}

/// The `LinkWalletCmd` payload: which token's cosmetic edition is being linked,
/// the player receiving the link, the custodial/WalletConnect wallet address,
/// the geo-check jurisdiction, and the serialized cosmetic edition's serial
/// number. Field names use the token marketplace's `camelCase` schema.
///
/// Build one directly and turn it into a [`Command`] with
/// [`LinkWalletCmd::into_command`], or decode it from a command payload via
/// [`serde_json`] inside [`CardToken::execute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkWalletCmd {
    /// The ERC-1155 token whose cosmetic edition is being linked; must name this
    /// CardToken.
    pub token_id: String,
    /// The player the wallet is being linked to.
    pub player_id: String,
    /// The custodial/WalletConnect wallet address being linked.
    pub wallet_address: String,
    /// The jurisdiction the link is geo-checked against.
    pub jurisdiction: String,
    /// Unique, non-reusable serial number for the cosmetic edition.
    pub serial_number: String,
}

impl LinkWalletCmd {
    /// The command name this maps to.
    pub const COMMAND: &'static str = LINK_WALLET;

    /// Build a command linking `wallet_address` to `player_id` for `token_id`'s
    /// cosmetic edition `serial_number`, geo-checked against `jurisdiction`.
    pub fn new(
        token_id: impl Into<String>,
        player_id: impl Into<String>,
        wallet_address: impl Into<String>,
        jurisdiction: impl Into<String>,
        serial_number: impl Into<String>,
    ) -> Self {
        Self {
            token_id: token_id.into(),
            player_id: player_id.into(),
            wallet_address: wallet_address.into(),
            jurisdiction: jurisdiction.into(),
            serial_number: serial_number.into(),
        }
    }

    /// Encode this command as a [`shared::Command`] carrying a JSON payload,
    /// ready to hand to [`CardToken::execute`].
    pub fn into_command(&self) -> Command {
        // Serialization of a plain data struct to a Vec cannot fail here.
        let payload = serde_json::to_vec(self).expect("LinkWalletCmd is always serializable");
        Command::with_payload(Self::COMMAND, payload)
    }
}

/// The wallet link that was recorded, carried by [`Event::WalletLinked`] and thus
/// by the emitted `wallet.linked` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletLinked {
    /// The ERC-1155 token whose cosmetic edition was linked.
    pub token_id: String,
    /// The player the wallet was linked to.
    pub player_id: String,
    /// The custodial/WalletConnect wallet address that was linked.
    pub wallet_address: String,
    /// The jurisdiction the link was geo-checked against.
    pub jurisdiction: String,
    /// Serialized cosmetic edition serial number.
    pub serial_number: String,
}

/// Domain events emitted by [`CardToken`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An ERC-1155 card token was minted with staged metadata.
    CardTokenMinted(CardTokenMinted),
    /// A card's IPFS metadata record was staged (pinned) for its tokenId.
    MetadataStaged(MetadataStaged),
    /// A custodial/WalletConnect wallet was linked to a player for a cosmetic
    /// edition.
    WalletLinked(WalletLinked),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::CardTokenMinted(_) => "card.token.minted",
            Event::MetadataStaged(_) => "metadata.staged",
            Event::WalletLinked(_) => "wallet.linked",
        }
    }
}

/// The CardToken aggregate: one mintable ERC-1155 card token.
///
/// Mirrors the shape produced by [`shared::stub_aggregate!`] (identity plus an
/// embedded [`AggregateRoot`]) so surrounding wiring stays consistent, while it
/// carries the state [`MintCardTokenCmd`] validates: whether ownership was
/// server-authoritatively verified, whether the staged IPFS metadata record is
/// resolvable, which serial numbers have already been consumed, and whether
/// on-chain ownership has been verified before an on-face cosmetic render.
#[derive(Debug)]
pub struct CardToken {
    id: String,
    root: AggregateRoot,
    /// Whether ownership was verified server-authoritatively at match start.
    ownership_verified_server_authoritatively: bool,
    /// Whether the tokenId maps to a resolvable IPFS metadata record.
    ipfs_metadata_record_resolvable: bool,
    /// Serial numbers already consumed by minted cosmetic editions.
    used_serial_numbers: Vec<String>,
    /// Whether on-chain ownership has been verified before on-face render.
    on_chain_ownership_verified_for_render: bool,
}

impl CardToken {
    /// Create a new, mint-ready CardToken identified by `id`: ownership is
    /// server-authoritatively verified, staged IPFS metadata is resolvable, no
    /// serial numbers have been consumed, and on-chain ownership has been
    /// verified for render. Use the configuration methods to drive it to the
    /// state a command validates.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: AggregateRoot::new(),
            ownership_verified_server_authoritatively: true,
            ipfs_metadata_record_resolvable: true,
            used_serial_numbers: Vec::new(),
            on_chain_ownership_verified_for_render: true,
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

    /// Record whether ownership was verified server-authoritatively at match
    /// start (`false` models client-asserted ownership).
    pub fn set_ownership_verified_server_authoritatively(&mut self, verified: bool) {
        self.ownership_verified_server_authoritatively = verified;
    }

    /// Record whether this tokenId maps to a resolvable IPFS metadata record.
    pub fn set_ipfs_metadata_record_resolvable(&mut self, resolvable: bool) {
        self.ipfs_metadata_record_resolvable = resolvable;
    }

    /// Mark a serial number as already consumed by a minted cosmetic edition.
    pub fn record_used_serial_number(&mut self, serial_number: impl Into<String>) {
        self.used_serial_numbers.push(serial_number.into());
    }

    /// Record whether on-chain ownership was verified before the cosmetic can
    /// render on-face.
    pub fn set_on_chain_ownership_verified_for_render(&mut self, verified: bool) {
        self.on_chain_ownership_verified_for_render = verified;
    }

    /// Server-authoritative ownership invariant: client-asserted ownership is
    /// never trusted.
    fn ensure_ownership_verified_server_authoritatively(&self) -> Result<(), DomainError> {
        if !self.ownership_verified_server_authoritatively {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' ownership was client-asserted; ownership is verified \
                 server-authoritatively at match start and client-asserted ownership is never trusted",
                self.id
            )));
        }
        Ok(())
    }

    /// Metadata invariant: every tokenId maps to a resolvable IPFS metadata
    /// record with all card render and rules fields.
    fn ensure_ipfs_metadata_record_resolvable(&self) -> Result<(), DomainError> {
        if !self.ipfs_metadata_record_resolvable {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' does not map to a resolvable IPFS metadata record; every tokenId \
                 maps to metadata containing name, cost, art URL, and effect ref",
                self.id
            )));
        }
        Ok(())
    }

    /// Serial invariant: serialized cosmetic editions carry a unique,
    /// non-reusable serial number.
    fn ensure_serial_number_unused(&self, serial_number: &str) -> Result<(), DomainError> {
        if self
            .used_serial_numbers
            .iter()
            .any(|used| used == serial_number)
        {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' serial number '{serial_number}' has already been used; serialized \
                 cosmetic editions carry a unique, non-reusable serial number",
                self.id
            )));
        }
        Ok(())
    }

    /// Render invariant: a cosmetic renders on-face only after verified on-chain
    /// ownership.
    fn ensure_on_chain_ownership_verified_for_render(&self) -> Result<(), DomainError> {
        if !self.on_chain_ownership_verified_for_render {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' would render a cosmetic before verified on-chain ownership; a \
                 cosmetic renders on-face only after verified on-chain ownership",
                self.id
            )));
        }
        Ok(())
    }

    /// Handle `MintCardTokenCmd`: verify the command carries a valid tokenId
    /// (naming this CardToken), staged IPFS metadata reference, and serial
    /// number; enforce every token marketplace invariant; mark the serial number
    /// consumed; and emit [`Event::CardTokenMinted`].
    fn mint_card_token(&mut self, cmd: MintCardTokenCmd) -> Result<Vec<Event>, DomainError> {
        if cmd.token_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a valid tokenId to mint",
                self.id
            )));
        }
        if cmd.ipfs_metadata_ref.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a valid ipfsMetadataRef to mint",
                self.id
            )));
        }
        if cmd.serial_number.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a valid serialNumber to mint",
                self.id
            )));
        }
        if cmd.token_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets card token '{}' but this aggregate is card token '{}'",
                cmd.token_id, self.id
            )));
        }

        self.ensure_ownership_verified_server_authoritatively()?;
        self.ensure_ipfs_metadata_record_resolvable()?;
        self.ensure_serial_number_unused(&cmd.serial_number)?;
        self.ensure_on_chain_ownership_verified_for_render()?;

        self.used_serial_numbers.push(cmd.serial_number.clone());

        let event = Event::CardTokenMinted(CardTokenMinted {
            token_id: cmd.token_id,
            ipfs_metadata_ref: cmd.ipfs_metadata_ref,
            serial_number: cmd.serial_number,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `StageMetadataCmd`: verify the command carries a valid tokenId
    /// (naming this CardToken), a resolvable metadata record, and a serial
    /// number; enforce every token marketplace invariant; mark the serial number
    /// consumed; and emit [`Event::MetadataStaged`].
    fn stage_metadata(&mut self, cmd: StageMetadataCmd) -> Result<Vec<Event>, DomainError> {
        if cmd.token_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a valid tokenId to stage metadata",
                self.id
            )));
        }
        if !cmd.metadata.is_resolvable() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a resolvable metadata record (name, cost, art URL, effect ref) to stage",
                self.id
            )));
        }
        if cmd.serial_number.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a valid serialNumber to stage metadata",
                self.id
            )));
        }
        if cmd.token_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets card token '{}' but this aggregate is card token '{}'",
                cmd.token_id, self.id
            )));
        }

        self.ensure_ownership_verified_server_authoritatively()?;
        self.ensure_ipfs_metadata_record_resolvable()?;
        self.ensure_serial_number_unused(&cmd.serial_number)?;
        self.ensure_on_chain_ownership_verified_for_render()?;

        self.used_serial_numbers.push(cmd.serial_number.clone());

        let event = Event::MetadataStaged(MetadataStaged {
            token_id: cmd.token_id,
            metadata: cmd.metadata,
            serial_number: cmd.serial_number,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }

    /// Handle `LinkWalletCmd`: verify the command carries a valid tokenId
    /// (naming this CardToken), playerId, wallet address, jurisdiction, and
    /// serial number; enforce every token marketplace invariant; mark the serial
    /// number consumed; and emit [`Event::WalletLinked`].
    fn link_wallet(&mut self, cmd: LinkWalletCmd) -> Result<Vec<Event>, DomainError> {
        if cmd.token_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a valid tokenId to link a wallet",
                self.id
            )));
        }
        if cmd.player_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a valid playerId to link a wallet",
                self.id
            )));
        }
        if cmd.wallet_address.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a valid walletAddress to link a wallet",
                self.id
            )));
        }
        if cmd.jurisdiction.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a valid jurisdiction to geo-check the wallet link",
                self.id
            )));
        }
        if cmd.serial_number.trim().is_empty() {
            return Err(DomainError::InvariantViolation(format!(
                "card token '{}' requires a valid serialNumber to link a wallet",
                self.id
            )));
        }
        if cmd.token_id != self.id {
            return Err(DomainError::InvariantViolation(format!(
                "command targets card token '{}' but this aggregate is card token '{}'",
                cmd.token_id, self.id
            )));
        }

        self.ensure_ownership_verified_server_authoritatively()?;
        self.ensure_ipfs_metadata_record_resolvable()?;
        self.ensure_serial_number_unused(&cmd.serial_number)?;
        self.ensure_on_chain_ownership_verified_for_render()?;

        self.used_serial_numbers.push(cmd.serial_number.clone());

        let event = Event::WalletLinked(WalletLinked {
            token_id: cmd.token_id,
            player_id: cmd.player_id,
            wallet_address: cmd.wallet_address,
            jurisdiction: cmd.jurisdiction,
            serial_number: cmd.serial_number,
        });
        self.root.record(Box::new(event.clone()));
        Ok(vec![event])
    }
}

impl Aggregate for CardToken {
    type Event = Event;

    fn aggregate_type() -> &'static str {
        AGGREGATE_TYPE
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError> {
        match command.name.as_str() {
            MINT_CARD_TOKEN => {
                let cmd: MintCardTokenCmd =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed MintCardTokenCmd payload: {e}"
                        ))
                    })?;
                self.mint_card_token(cmd)
            }
            STAGE_METADATA => {
                let cmd: StageMetadataCmd =
                    serde_json::from_slice(&command.payload).map_err(|e| {
                        DomainError::InvariantViolation(format!(
                            "malformed StageMetadataCmd payload: {e}"
                        ))
                    })?;
                self.stage_metadata(cmd)
            }
            LINK_WALLET => {
                let cmd: LinkWalletCmd = serde_json::from_slice(&command.payload).map_err(|e| {
                    DomainError::InvariantViolation(format!("malformed LinkWalletCmd payload: {e}"))
                })?;
                self.link_wallet(cmd)
            }
            // Any other command is unknown to this aggregate.
            _ => Err(DomainError::unknown_command(
                <Self as Aggregate>::aggregate_type(),
                command.name,
            )),
        }
    }
}

/// Repository contract for the [`CardToken`] aggregate. Adapters implement
/// [`shared::Repository`] for `CardToken` and then this marker trait.
pub trait CardTokenRepository: Repository<CardToken> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mint-ready CardToken `token-01`: server-authoritative ownership,
    /// resolvable staged metadata, no consumed serials, and verified on-chain
    /// ownership for on-face render.
    fn ready_token() -> CardToken {
        let mut token = CardToken::new("token-01");
        token.set_ownership_verified_server_authoritatively(true);
        token.set_ipfs_metadata_record_resolvable(true);
        token.set_on_chain_ownership_verified_for_render(true);
        token
    }

    /// A command minting token `token-01` with staged IPFS metadata and serial
    /// `SN-0001`.
    fn valid_cmd() -> MintCardTokenCmd {
        MintCardTokenCmd::new("token-01", "ipfs://metadata/token-01", "SN-0001")
    }

    // Scenario: Successfully execute MintCardTokenCmd.
    #[test]
    fn mints_and_emits_card_token_minted_event() {
        let mut token = ready_token();

        let events = token
            .execute(valid_cmd().into_command())
            .expect("valid mint should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "card.token.minted");
        match &events[0] {
            Event::CardTokenMinted(minted) => {
                assert_eq!(minted.token_id, "token-01");
                assert_eq!(minted.ipfs_metadata_ref, "ipfs://metadata/token-01");
                assert_eq!(minted.serial_number, "SN-0001");
            }
            other => panic!("expected CardTokenMinted, got {other:?}"),
        }
        // The CardToken recorded the event and consumed the serial number.
        assert_eq!(token.version(), 1);
        assert_eq!(token.uncommitted_events().len(), 1);
        assert_eq!(
            token.uncommitted_events()[0].event_type(),
            "card.token.minted"
        );
    }

    // Scenario: rejected - Ownership is verified server-authoritatively at match
    // start; client-asserted ownership is never trusted.
    #[test]
    fn rejects_when_ownership_not_server_authoritative() {
        let mut token = ready_token();
        token.set_ownership_verified_server_authoritatively(false);

        let err = token
            .execute(valid_cmd().into_command())
            .expect_err("client-asserted ownership must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Scenario: rejected - Every tokenId maps to a resolvable IPFS metadata
    // record (name, cost, art URL, effect ref).
    #[test]
    fn rejects_when_ipfs_metadata_record_is_not_resolvable() {
        let mut token = ready_token();
        token.set_ipfs_metadata_record_resolvable(false);

        let err = token
            .execute(valid_cmd().into_command())
            .expect_err("unresolvable staged metadata must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Scenario: rejected - Serialized cosmetic editions carry a unique,
    // non-reusable serial number.
    #[test]
    fn rejects_when_serial_number_was_already_used() {
        let mut token = ready_token();
        token.record_used_serial_number("SN-0001");

        let err = token
            .execute(valid_cmd().into_command())
            .expect_err("a reused serial number must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Serialized cosmetic editions are non-reusable in practice: a second mint
    // with the same serial is rejected after the first successful mint consumed
    // it.
    #[test]
    fn rejects_repeated_mint_with_same_serial_number() {
        let mut token = ready_token();

        token
            .execute(valid_cmd().into_command())
            .expect("first mint should succeed");
        let err = token
            .execute(valid_cmd().into_command())
            .expect_err("a repeated serial number must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 1);
        assert_eq!(token.uncommitted_events().len(), 1);
    }

    // Scenario: rejected - A cosmetic renders on-face only after verified
    // on-chain ownership.
    #[test]
    fn rejects_when_on_chain_ownership_not_verified_for_render() {
        let mut token = ready_token();
        token.set_on_chain_ownership_verified_for_render(false);

        let err = token
            .execute(valid_cmd().into_command())
            .expect_err("unverified on-chain ownership must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // A command naming a different CardToken is rejected before any invariant
    // runs.
    #[test]
    fn rejects_command_for_a_different_token() {
        let mut token = ready_token();
        let cmd = MintCardTokenCmd::new("token-99", "ipfs://metadata/token-99", "SN-0001");

        let err = token
            .execute(cmd.into_command())
            .expect_err("a command for another token must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Commands missing any required field are rejected.
    #[test]
    fn rejects_command_with_missing_fields() {
        for cmd in [
            MintCardTokenCmd::new("   ", "ipfs://metadata/token-01", "SN-0001"),
            MintCardTokenCmd::new("token-01", "   ", "SN-0001"),
            MintCardTokenCmd::new("token-01", "ipfs://metadata/token-01", "   "),
        ] {
            let mut token = ready_token();
            let err = token
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(token.version(), 0);
        }
    }

    // An unrecognized command is still an UnknownCommand for this aggregate,
    // preserving the contract the mock adapters rely on.
    #[test]
    fn rejects_unknown_command() {
        let mut token = CardToken::new("token-01");
        let err = token.execute(Command::new("NoSuchCommand")).unwrap_err();
        match err {
            DomainError::UnknownCommand { aggregate, command } => {
                assert_eq!(aggregate, "CardToken");
                assert_eq!(command, "NoSuchCommand");
            }
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn command_payload_round_trips() {
        let cmd = valid_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, MintCardTokenCmd::COMMAND);
        let decoded: MintCardTokenCmd = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_cmd());
    }

    // ---- StageMetadataCmd (S-61) --------------------------------------------

    /// A resolvable metadata record for `token-01`.
    fn valid_metadata() -> StagedMetadata {
        StagedMetadata::new(
            "Ember Drake",
            4,
            "ipfs://art/token-01",
            "effect://ember-drake",
        )
    }

    /// A command staging valid metadata for `token-01` under serial `SN-0001`.
    fn valid_stage_cmd() -> StageMetadataCmd {
        StageMetadataCmd::new("token-01", valid_metadata(), "SN-0001")
    }

    // Scenario: Successfully execute StageMetadataCmd.
    #[test]
    fn stages_and_emits_metadata_staged_event() {
        let mut token = ready_token();

        let events = token
            .execute(valid_stage_cmd().into_command())
            .expect("valid stage should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "metadata.staged");
        match &events[0] {
            Event::MetadataStaged(staged) => {
                assert_eq!(staged.token_id, "token-01");
                assert_eq!(staged.metadata, valid_metadata());
                assert_eq!(staged.serial_number, "SN-0001");
            }
            other => panic!("expected MetadataStaged, got {other:?}"),
        }
        assert_eq!(token.version(), 1);
        assert_eq!(token.uncommitted_events().len(), 1);
        assert_eq!(
            token.uncommitted_events()[0].event_type(),
            "metadata.staged"
        );
    }

    // Scenario: rejected - Ownership is verified server-authoritatively at match
    // start; client-asserted ownership is never trusted.
    #[test]
    fn stage_rejects_when_ownership_not_server_authoritative() {
        let mut token = ready_token();
        token.set_ownership_verified_server_authoritatively(false);

        let err = token
            .execute(valid_stage_cmd().into_command())
            .expect_err("client-asserted ownership must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Scenario: rejected - Every tokenId maps to a resolvable IPFS metadata
    // record (name, cost, art URL, effect ref).
    #[test]
    fn stage_rejects_when_ipfs_metadata_record_is_not_resolvable() {
        let mut token = ready_token();
        token.set_ipfs_metadata_record_resolvable(false);

        let err = token
            .execute(valid_stage_cmd().into_command())
            .expect_err("unresolvable staged metadata must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Scenario: rejected - Serialized cosmetic editions carry a unique,
    // non-reusable serial number.
    #[test]
    fn stage_rejects_when_serial_number_was_already_used() {
        let mut token = ready_token();
        token.record_used_serial_number("SN-0001");

        let err = token
            .execute(valid_stage_cmd().into_command())
            .expect_err("a reused serial number must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Scenario: rejected - A cosmetic renders on-face only after verified
    // on-chain ownership.
    #[test]
    fn stage_rejects_when_on_chain_ownership_not_verified_for_render() {
        let mut token = ready_token();
        token.set_on_chain_ownership_verified_for_render(false);

        let err = token
            .execute(valid_stage_cmd().into_command())
            .expect_err("unverified on-chain ownership must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // A stage command naming a different CardToken is rejected before any
    // invariant runs.
    #[test]
    fn stage_rejects_command_for_a_different_token() {
        let mut token = ready_token();
        let cmd = StageMetadataCmd::new("token-99", valid_metadata(), "SN-0001");

        let err = token
            .execute(cmd.into_command())
            .expect_err("a command for another token must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Stage commands missing any required field (including an unresolvable
    // metadata record) are rejected.
    #[test]
    fn stage_rejects_command_with_missing_fields() {
        let cmds = [
            StageMetadataCmd::new("   ", valid_metadata(), "SN-0001"),
            StageMetadataCmd::new(
                "token-01",
                StagedMetadata::new("  ", 4, "ipfs://art", "eff"),
                "SN-0001",
            ),
            StageMetadataCmd::new(
                "token-01",
                StagedMetadata::new("Ember", 4, "  ", "eff"),
                "SN-0001",
            ),
            StageMetadataCmd::new(
                "token-01",
                StagedMetadata::new("Ember", 4, "ipfs://art", "  "),
                "SN-0001",
            ),
            StageMetadataCmd::new("token-01", valid_metadata(), "   "),
        ];
        for cmd in cmds {
            let mut token = ready_token();
            let err = token
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(token.version(), 0);
        }
    }

    #[test]
    fn stage_command_payload_round_trips() {
        let cmd = valid_stage_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, StageMetadataCmd::COMMAND);
        let decoded: StageMetadataCmd = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_stage_cmd());
    }

    // ---- LinkWalletCmd (S-62) -----------------------------------------------

    /// A command linking wallet `0xWALLET` to player `player-7` for `token-01`'s
    /// cosmetic edition `SN-0001`, geo-checked against jurisdiction `US-CA`.
    fn valid_link_cmd() -> LinkWalletCmd {
        LinkWalletCmd::new("token-01", "player-7", "0xWALLET", "US-CA", "SN-0001")
    }

    // Scenario: Successfully execute LinkWalletCmd.
    #[test]
    fn links_wallet_and_emits_wallet_linked_event() {
        let mut token = ready_token();

        let events = token
            .execute(valid_link_cmd().into_command())
            .expect("valid link should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "wallet.linked");
        match &events[0] {
            Event::WalletLinked(linked) => {
                assert_eq!(linked.token_id, "token-01");
                assert_eq!(linked.player_id, "player-7");
                assert_eq!(linked.wallet_address, "0xWALLET");
                assert_eq!(linked.jurisdiction, "US-CA");
                assert_eq!(linked.serial_number, "SN-0001");
            }
            other => panic!("expected WalletLinked, got {other:?}"),
        }
        assert_eq!(token.version(), 1);
        assert_eq!(token.uncommitted_events().len(), 1);
        assert_eq!(token.uncommitted_events()[0].event_type(), "wallet.linked");
    }

    // Scenario: rejected - Ownership is verified server-authoritatively at match
    // start; client-asserted ownership is never trusted.
    #[test]
    fn link_rejects_when_ownership_not_server_authoritative() {
        let mut token = ready_token();
        token.set_ownership_verified_server_authoritatively(false);

        let err = token
            .execute(valid_link_cmd().into_command())
            .expect_err("client-asserted ownership must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Scenario: rejected - Every tokenId maps to a resolvable IPFS metadata
    // record (name, cost, art URL, effect ref).
    #[test]
    fn link_rejects_when_ipfs_metadata_record_is_not_resolvable() {
        let mut token = ready_token();
        token.set_ipfs_metadata_record_resolvable(false);

        let err = token
            .execute(valid_link_cmd().into_command())
            .expect_err("unresolvable staged metadata must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Scenario: rejected - Serialized cosmetic editions carry a unique,
    // non-reusable serial number.
    #[test]
    fn link_rejects_when_serial_number_was_already_used() {
        let mut token = ready_token();
        token.record_used_serial_number("SN-0001");

        let err = token
            .execute(valid_link_cmd().into_command())
            .expect_err("a reused serial number must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Scenario: rejected - A cosmetic renders on-face only after verified
    // on-chain ownership.
    #[test]
    fn link_rejects_when_on_chain_ownership_not_verified_for_render() {
        let mut token = ready_token();
        token.set_on_chain_ownership_verified_for_render(false);

        let err = token
            .execute(valid_link_cmd().into_command())
            .expect_err("unverified on-chain ownership must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // A link command naming a different CardToken is rejected before any
    // invariant runs.
    #[test]
    fn link_rejects_command_for_a_different_token() {
        let mut token = ready_token();
        let cmd = LinkWalletCmd::new("token-99", "player-7", "0xWALLET", "US-CA", "SN-0001");

        let err = token
            .execute(cmd.into_command())
            .expect_err("a command for another token must be rejected");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
        assert_eq!(token.version(), 0);
    }

    // Link commands missing any required field are rejected.
    #[test]
    fn link_rejects_command_with_missing_fields() {
        let cmds = [
            LinkWalletCmd::new("   ", "player-7", "0xWALLET", "US-CA", "SN-0001"),
            LinkWalletCmd::new("token-01", "   ", "0xWALLET", "US-CA", "SN-0001"),
            LinkWalletCmd::new("token-01", "player-7", "   ", "US-CA", "SN-0001"),
            LinkWalletCmd::new("token-01", "player-7", "0xWALLET", "   ", "SN-0001"),
            LinkWalletCmd::new("token-01", "player-7", "0xWALLET", "US-CA", "   "),
        ];
        for cmd in cmds {
            let mut token = ready_token();
            let err = token
                .execute(cmd.into_command())
                .expect_err("a command with a missing field must be rejected");
            assert!(matches!(err, DomainError::InvariantViolation(_)));
            assert_eq!(token.version(), 0);
        }
    }

    #[test]
    fn link_command_payload_round_trips() {
        let cmd = valid_link_cmd();
        let command = cmd.into_command();
        assert_eq!(command.name, LinkWalletCmd::COMMAND);
        let decoded: LinkWalletCmd = serde_json::from_slice(&command.payload).unwrap();
        assert_eq!(decoded, valid_link_cmd());
    }
}
