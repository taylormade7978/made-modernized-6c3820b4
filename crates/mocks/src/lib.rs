//! In-memory mock repository adapters.
//!
//! These are *driven adapters* on the persistence side of the hexagon: each
//! implements the [`shared::Repository`] port plus the per-aggregate repository
//! contract defined by its bounded context (e.g. `SeasonRepository`). They back
//! the store with a `HashMap`, so the test suite — and early wiring of the
//! actix-web server — can exercise the domain without a real database.

use std::collections::HashMap;

use shared::{DomainError, Repository};

/// Generate an in-memory [`Repository`] adapter for an aggregate and assert it
/// satisfies that aggregate's repository contract.
macro_rules! in_memory_repo {
    ($(#[$doc:meta])* $name:ident, $agg:path, $repo:path) => {
        $(#[$doc])*
        #[derive(Default)]
        pub struct $name {
            store: HashMap<String, $agg>,
        }

        impl $name {
            /// A new, empty in-memory repository.
            pub fn new() -> Self {
                Self::default()
            }

            /// Number of stored aggregates.
            pub fn len(&self) -> usize {
                self.store.len()
            }

            /// Whether the store holds no aggregates.
            pub fn is_empty(&self) -> bool {
                self.store.is_empty()
            }
        }

        impl Repository<$agg> for $name {
            fn find_by_id(&self, id: &str) -> Result<Option<&$agg>, DomainError> {
                Ok(self.store.get(id))
            }

            fn save(&mut self, id: &str, aggregate: $agg) -> Result<(), DomainError> {
                self.store.insert(id.to_string(), aggregate);
                Ok(())
            }
        }

        // Compile-time proof the mock fulfills the domain repository contract.
        impl $repo for $name {}
    };
}

in_memory_repo!(
    /// In-memory adapter for [`game_session::GameSessionRepository`].
    InMemoryGameSessionRepository,
    game_session::GameSession,
    game_session::GameSessionRepository
);
in_memory_repo!(
    /// In-memory adapter for [`domain::match_replay::MatchReplayRepository`].
    InMemoryMatchReplayRepository,
    domain::match_replay::MatchReplay,
    domain::match_replay::MatchReplayRepository
);
in_memory_repo!(
    /// In-memory adapter for [`domain::card_definition::CardDefinitionRepository`].
    InMemoryCardDefinitionRepository,
    domain::card_definition::CardDefinition,
    domain::card_definition::CardDefinitionRepository
);
in_memory_repo!(
    /// In-memory adapter for [`domain::card_token::CardTokenRepository`].
    InMemoryCardTokenRepository,
    domain::card_token::CardToken,
    domain::card_token::CardTokenRepository
);
in_memory_repo!(
    /// In-memory adapter for [`domain::boss_definition::BossDefinitionRepository`].
    InMemoryBossDefinitionRepository,
    domain::boss_definition::BossDefinition,
    domain::boss_definition::BossDefinitionRepository
);
in_memory_repo!(
    /// In-memory adapter for [`domain::expansion_set::ExpansionSetRepository`].
    InMemoryExpansionSetRepository,
    domain::expansion_set::ExpansionSet,
    domain::expansion_set::ExpansionSetRepository
);
in_memory_repo!(
    /// In-memory adapter for [`domain::matchmaking_ticket::MatchmakingTicketRepository`].
    InMemoryMatchmakingTicketRepository,
    domain::matchmaking_ticket::MatchmakingTicket,
    domain::matchmaking_ticket::MatchmakingTicketRepository
);
in_memory_repo!(
    /// In-memory adapter for [`domain::mission_attempt::MissionAttemptRepository`].
    InMemoryMissionAttemptRepository,
    domain::mission_attempt::MissionAttempt,
    domain::mission_attempt::MissionAttemptRepository
);
in_memory_repo!(
    /// In-memory adapter for [`domain::ranked_standing::RankedStandingRepository`].
    InMemoryRankedStandingRepository,
    domain::ranked_standing::RankedStanding,
    domain::ranked_standing::RankedStandingRepository
);
in_memory_repo!(
    /// In-memory adapter for [`domain::season::SeasonRepository`].
    InMemorySeasonRepository,
    domain::season::Season,
    domain::season::SeasonRepository
);
in_memory_repo!(
    /// In-memory adapter for [`domain::player_collection::PlayerCollectionRepository`].
    InMemoryPlayerCollectionRepository,
    domain::player_collection::PlayerCollection,
    domain::player_collection::PlayerCollectionRepository
);

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{Aggregate, Command, DomainError};

    #[test]
    fn mock_repository_saves_and_loads_an_aggregate() {
        let mut repo = InMemorySeasonRepository::new();
        assert!(repo.is_empty());

        let season = domain::season::Season::new("2026-summer");
        repo.save("2026-summer", season).unwrap();

        assert_eq!(repo.len(), 1);
        let loaded = repo.find_by_id("2026-summer").unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id(), "2026-summer");
        assert!(repo.find_by_id("missing").unwrap().is_none());
    }

    #[test]
    fn fresh_aggregate_starts_at_version_zero_with_no_events() {
        let session = game_session::GameSession::new("s-1");
        assert_eq!(session.version(), 0);
        assert!(session.uncommitted_events().is_empty());
    }

    /// Every bounded-context aggregate must reject an unrecognized command with
    /// [`DomainError::UnknownCommand`], naming itself. This drives `execute` on
    /// each of the eight stubs.
    #[test]
    fn every_aggregate_rejects_unknown_commands() {
        macro_rules! assert_unknown {
            ($ctor:expr, $expected:literal) => {{
                let mut agg = $ctor;
                let err = agg.execute(Command::new("NoSuchCommand")).unwrap_err();
                match err {
                    DomainError::UnknownCommand { aggregate, command } => {
                        assert_eq!(aggregate, $expected);
                        assert_eq!(command, "NoSuchCommand");
                    }
                    other => panic!("expected UnknownCommand, got {other:?}"),
                }
            }};
        }

        assert_unknown!(game_session::GameSession::new("g"), "GameSession");
        assert_unknown!(domain::match_replay::MatchReplay::new("m"), "MatchReplay");
        assert_unknown!(
            domain::card_definition::CardDefinition::new("c"),
            "CardDefinition"
        );
        assert_unknown!(domain::card_token::CardToken::new("ct"), "CardToken");
        assert_unknown!(
            domain::boss_definition::BossDefinition::new("b"),
            "BossDefinition"
        );
        assert_unknown!(
            domain::expansion_set::ExpansionSet::new("e"),
            "ExpansionSet"
        );
        assert_unknown!(
            domain::matchmaking_ticket::MatchmakingTicket::new("t"),
            "MatchmakingTicket"
        );
        assert_unknown!(
            domain::mission_attempt::MissionAttempt::new("ma"),
            "MissionAttempt"
        );
        assert_unknown!(
            domain::ranked_standing::RankedStanding::new("r"),
            "RankedStanding"
        );
        assert_unknown!(domain::season::Season::new("s"), "Season");
        assert_unknown!(
            domain::player_collection::PlayerCollection::new("p"),
            "PlayerCollection"
        );
    }

    /// Exercise every mock repository through its contract so the whole
    /// persistence surface is covered by the local test run.
    #[test]
    fn all_mock_repositories_round_trip() {
        fn round_trip<A>(repo: &mut impl Repository<A>, id: &str, agg: A) {
            repo.save(id, agg).unwrap();
            assert!(repo.find_by_id(id).unwrap().is_some());
        }

        round_trip(
            &mut InMemoryGameSessionRepository::new(),
            "g",
            game_session::GameSession::new("g"),
        );
        round_trip(
            &mut InMemoryMatchReplayRepository::new(),
            "m",
            domain::match_replay::MatchReplay::new("m"),
        );
        round_trip(
            &mut InMemoryCardDefinitionRepository::new(),
            "c",
            domain::card_definition::CardDefinition::new("c"),
        );
        round_trip(
            &mut InMemoryCardTokenRepository::new(),
            "ct",
            domain::card_token::CardToken::new("ct"),
        );
        round_trip(
            &mut InMemoryBossDefinitionRepository::new(),
            "b",
            domain::boss_definition::BossDefinition::new("b"),
        );
        round_trip(
            &mut InMemoryExpansionSetRepository::new(),
            "e",
            domain::expansion_set::ExpansionSet::new("e"),
        );
        round_trip(
            &mut InMemoryMatchmakingTicketRepository::new(),
            "t",
            domain::matchmaking_ticket::MatchmakingTicket::new("t"),
        );
        round_trip(
            &mut InMemoryMissionAttemptRepository::new(),
            "ma",
            domain::mission_attempt::MissionAttempt::new("ma"),
        );
        round_trip(
            &mut InMemoryRankedStandingRepository::new(),
            "r",
            domain::ranked_standing::RankedStanding::new("r"),
        );
        round_trip(
            &mut InMemorySeasonRepository::new(),
            "s",
            domain::season::Season::new("s"),
        );
        round_trip(
            &mut InMemoryPlayerCollectionRepository::new(),
            "p",
            domain::player_collection::PlayerCollection::new("p"),
        );
    }
}
