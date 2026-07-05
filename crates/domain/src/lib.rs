//! Bounded-context aggregates for MADE (everything except GameSession, which
//! lives in its own WASM-capable crate).
//!
//! Each context is a self-contained module that scaffolds its aggregate and
//! repository contract via [`shared::stub_aggregate!`]. Grouping the
//! non-GameSession contexts here keeps the domain layer cohesive while the
//! GameSession rules stay isolated for their dual native/WASM build.

pub mod boss_definition;
pub mod card_definition;
pub mod expansion_set;
pub mod match_replay;
pub mod matchmaking_ticket;
pub mod outfit;
pub mod player_collection;
pub mod ranked_standing;
pub mod season;
