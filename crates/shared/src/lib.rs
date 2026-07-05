//! Shared domain kernel for the MADE hexagonal architecture.
//!
//! This crate is the *domain core* of the hexagon. It defines the contracts
//! (ports) that every bounded context implements and that the outer adapters
//! (the actix-web server, in-memory mocks, a future event store, …) depend on:
//!
//! * [`Aggregate`] — the command-handling contract with an `execute(cmd)` op.
//! * [`AggregateRoot`] — a base type embedded by every aggregate that tracks
//!   the aggregate `version` and the list of `uncommitted` [`DomainEvent`]s.
//! * [`DomainEvent`] — the contract every emitted event satisfies.
//! * [`DomainError`] — domain-level failures, including [`DomainError::UnknownCommand`].
//! * [`Repository`] — the persistence port, one contract implemented per aggregate.
//!
//! The kernel deliberately has **zero external dependencies** so it is safe to
//! compile to `wasm32` (the GameSession rules crate links it for shared
//! server/client execution) and keeps the domain free of framework concerns.

use std::error::Error;
use std::fmt;

/// A command dispatched to an [`Aggregate`] via [`Aggregate::execute`].
///
/// Commands are carried as a named message with an opaque payload. Concrete
/// aggregates match on [`Command::name`] to route to a handler; a name that no
/// aggregate recognizes yields [`DomainError::UnknownCommand`]. As real
/// behavior is added, aggregates simply grow their set of recognized names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The command type name, e.g. `"StartMatch"`. Used for routing.
    pub name: String,
    /// Opaque, serialization-agnostic payload (JSON/CBOR/etc. bytes).
    pub payload: Vec<u8>,
}

impl Command {
    /// Build a payload-less command with the given type name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            payload: Vec::new(),
        }
    }

    /// Build a command carrying an opaque payload.
    pub fn with_payload(name: impl Into<String>, payload: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            payload,
        }
    }
}

/// Contract satisfied by every domain event an aggregate emits.
///
/// Events are the only record of a state change; the [`AggregateRoot`] keeps
/// them as *uncommitted* until an adapter persists them.
///
/// `Send + Sync` are required because the authoritative server moves aggregates
/// (and their buffered events) across async worker threads; on `wasm32` the
/// bounds are trivially satisfied.
pub trait DomainEvent: fmt::Debug + Send + Sync {
    /// Stable, serialization-friendly event type name, e.g. `"match.started"`.
    fn event_type(&self) -> &'static str;
}

/// Domain-level failures raised while handling a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    /// The aggregate received a command it does not recognize.
    UnknownCommand {
        /// The aggregate type that rejected the command.
        aggregate: &'static str,
        /// The unrecognized command name.
        command: String,
    },
    /// A business invariant was violated; carries a human-readable reason.
    InvariantViolation(String),
}

impl DomainError {
    /// Construct an [`DomainError::UnknownCommand`] for the given aggregate/command.
    pub fn unknown_command(aggregate: &'static str, command: impl Into<String>) -> Self {
        DomainError::UnknownCommand {
            aggregate,
            command: command.into(),
        }
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DomainError::UnknownCommand { aggregate, command } => {
                write!(f, "unknown command '{command}' for aggregate '{aggregate}'")
            }
            DomainError::InvariantViolation(reason) => {
                write!(f, "invariant violation: {reason}")
            }
        }
    }
}

impl Error for DomainError {}

/// Base type embedded by every aggregate to track optimistic-concurrency
/// `version` and the events produced-but-not-yet-persisted (`uncommitted`).
///
/// An aggregate calls [`AggregateRoot::record`] for each event it emits, which
/// appends the event and bumps the version. An adapter that persists the events
/// then calls [`AggregateRoot::mark_committed`] to clear the buffer.
#[derive(Debug, Default)]
pub struct AggregateRoot {
    version: u64,
    uncommitted: Vec<Box<dyn DomainEvent>>,
}

impl AggregateRoot {
    /// A fresh root at version 0 with no uncommitted events.
    pub fn new() -> Self {
        Self {
            version: 0,
            uncommitted: Vec::new(),
        }
    }

    /// Current aggregate version (number of committed + uncommitted events).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Record a newly-produced event as uncommitted and advance the version.
    pub fn record(&mut self, event: Box<dyn DomainEvent>) {
        self.uncommitted.push(event);
        self.version += 1;
    }

    /// Events produced since the last commit, in emission order.
    pub fn uncommitted_events(&self) -> &[Box<dyn DomainEvent>] {
        &self.uncommitted
    }

    /// Drop the uncommitted buffer after an adapter has persisted the events.
    pub fn mark_committed(&mut self) {
        self.uncommitted.clear();
    }
}

/// The command-handling contract every aggregate implements.
///
/// `execute(cmd)` is the single write entrypoint: it validates the command,
/// checks invariants, records events on the [`AggregateRoot`], and returns the
/// events produced — or a [`DomainError`] such as [`DomainError::UnknownCommand`].
pub trait Aggregate {
    /// The concrete event type this aggregate emits.
    type Event: DomainEvent;

    /// Stable aggregate type name, used in errors and event routing.
    fn aggregate_type() -> &'static str
    where
        Self: Sized;

    /// Handle a command: mutate state, record events, and return them.
    fn execute(&mut self, command: Command) -> Result<Vec<Self::Event>, DomainError>;
}

/// The persistence port for an aggregate `A`. One contract is implemented per
/// aggregate (see the per-aggregate marker traits), and adapters — the
/// in-memory mocks, or a real store — provide the implementations.
pub trait Repository<A> {
    /// Load an aggregate by its identity, if present.
    fn find_by_id(&self, id: &str) -> Result<Option<&A>, DomainError>;

    /// Persist an aggregate under the given identity.
    fn save(&mut self, id: &str, aggregate: A) -> Result<(), DomainError>;
}

/// Generate a bounded-context aggregate stub plus its repository contract.
///
/// Every bounded context is scaffolded identically at this stage: an aggregate
/// struct embedding [`AggregateRoot`], an (as-yet uninhabited) `Event` enum, an
/// [`Aggregate`] impl whose `execute` recognizes no commands (returning
/// [`DomainError::UnknownCommand`]), and a repository marker trait extending
/// [`Repository`]. As real behavior arrives, a context replaces its
/// `stub_aggregate!` invocation with hand-written command/event handling.
///
/// Invoke once per aggregate module: `stub_aggregate!(Season, SeasonRepository);`
#[macro_export]
macro_rules! stub_aggregate {
    ($agg:ident, $repo:ident) => {
        /// Bounded-context aggregate stub. Embeds [`$crate::AggregateRoot`] for
        /// version and uncommitted-event tracking; no commands handled yet.
        #[derive(Debug)]
        pub struct $agg {
            id: ::std::string::String,
            root: $crate::AggregateRoot,
        }

        impl $agg {
            /// Create a new aggregate instance with the given identity.
            pub fn new(id: impl ::std::convert::Into<::std::string::String>) -> Self {
                Self {
                    id: id.into(),
                    root: $crate::AggregateRoot::new(),
                }
            }

            /// This aggregate's identity.
            pub fn id(&self) -> &str {
                &self.id
            }

            /// Current version (delegates to the embedded [`$crate::AggregateRoot`]).
            pub fn version(&self) -> u64 {
                self.root.version()
            }

            /// Events produced but not yet persisted.
            pub fn uncommitted_events(&self) -> &[::std::boxed::Box<dyn $crate::DomainEvent>] {
                self.root.uncommitted_events()
            }
        }

        /// Domain events for this aggregate. Uninhabited for now — the stub
        /// emits no events; real variants arrive alongside behavior.
        #[derive(Debug)]
        pub enum Event {}

        impl $crate::DomainEvent for Event {
            fn event_type(&self) -> &'static str {
                // Unreachable: the enum has no variants, so no value exists.
                match *self {}
            }
        }

        impl $crate::Aggregate for $agg {
            type Event = Event;

            fn aggregate_type() -> &'static str {
                ::std::stringify!($agg)
            }

            fn execute(
                &mut self,
                command: $crate::Command,
            ) -> ::std::result::Result<::std::vec::Vec<Self::Event>, $crate::DomainError> {
                // Stub behavior: no command names are recognized yet, so every
                // command is an UnknownCommand for this aggregate.
                ::std::result::Result::Err($crate::DomainError::unknown_command(
                    <Self as $crate::Aggregate>::aggregate_type(),
                    command.name,
                ))
            }
        }

        /// Repository contract for the `$agg` aggregate. Adapters implement
        /// [`$crate::Repository`] for `$agg` and then this marker trait.
        pub trait $repo: $crate::Repository<$agg> {}
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct Started;
    impl DomainEvent for Started {
        fn event_type(&self) -> &'static str {
            "test.started"
        }
    }

    #[test]
    fn aggregate_root_tracks_version_and_uncommitted_events() {
        let mut root = AggregateRoot::new();
        assert_eq!(root.version(), 0);
        assert!(root.uncommitted_events().is_empty());

        root.record(Box::new(Started));
        root.record(Box::new(Started));
        assert_eq!(root.version(), 2);
        assert_eq!(root.uncommitted_events().len(), 2);

        root.mark_committed();
        assert!(root.uncommitted_events().is_empty());
        // Version is monotonic: committing does not rewind it.
        assert_eq!(root.version(), 2);
    }

    #[test]
    fn unknown_command_error_carries_aggregate_and_command() {
        let err = DomainError::unknown_command("GameSession", "Frobnicate");
        assert_eq!(
            err,
            DomainError::UnknownCommand {
                aggregate: "GameSession",
                command: "Frobnicate".to_string(),
            }
        );
        assert_eq!(
            err.to_string(),
            "unknown command 'Frobnicate' for aggregate 'GameSession'"
        );
    }
}
