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
//! on success emits [`Event::CardTokenMinted`] (`card.token.minted`). This
//! module is hand-written (it does not use `shared::stub_aggregate!`) but
//! preserves the same public surface as the scaffolded contexts: a
//! [`CardToken`] aggregate and a [`CardTokenRepository`] port.

use serde::{Deserialize, Serialize};

use shared::{Aggregate, AggregateRoot, Command, DomainError, DomainEvent, Repository};

/// Stable aggregate type name, surfaced in [`DomainError::UnknownCommand`] and
/// used for command routing.
const AGGREGATE_TYPE: &str = "CardToken";

/// The command name [`CardToken::execute`] recognizes to mint an ERC-1155 card
/// token with staged metadata.
const MINT_CARD_TOKEN: &str = "MintCardTokenCmd";

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

/// Domain events emitted by [`CardToken`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An ERC-1155 card token was minted with staged metadata.
    CardTokenMinted(CardTokenMinted),
}

impl DomainEvent for Event {
    fn event_type(&self) -> &'static str {
        match self {
            Event::CardTokenMinted(_) => "card.token.minted",
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
}
