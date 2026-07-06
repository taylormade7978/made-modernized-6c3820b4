/**
 * Client-side model of a live `GameSession`.
 *
 * These types mirror the authoritative Rust rules aggregate in
 * `crates/game-session` (its `OutfitConfig`, `Player`, command payloads, and
 * emitted `Event`s) expressed as idiomatic TypeScript. Because the WebSocket
 * match protocol is the contract between the PWA and the authoritative game
 * server — exactly as `api/types.ts` is for the REST surface — these interfaces
 * are the client-side source of truth for the on-the-wire match payloads; keep
 * them in step with the domain structs as the socket protocol lands.
 *
 * The rules *constants* below are copied verbatim from the crate's `pub const`
 * declarations. They let the browser predict a move's legality locally (the
 * optimistic UI) with the same numbers the server enforces, so a prediction and
 * the authoritative decision only diverge on genuinely server-only knowledge
 * (hidden deck contents, the seeded RNG), which reconciliation then corrects.
 */

// ── Rules constants (mirror `crates/game-session/src/lib.rs`) ──────────────────

/** Heat a player gains each time they play a card (`HEAT_PER_PLAY`). */
export const HEAT_PER_PLAY = 1
/** A board may hold at most this many Operators at once (`MAX_OPERATORS`). */
export const MAX_OPERATORS = 7
/** A board may hold at most this many Vehicles at once (`MAX_VEHICLES`). */
export const MAX_VEHICLES = 3
/** Juice a player opens a match with (`STARTING_JUICE`). */
export const STARTING_JUICE = 1
/** Juice is hard-capped at this value; no state may exceed it (`JUICE_CAP`). */
export const JUICE_CAP = 10
/** Juice a seat gains at the start of each of its turns (`JUICE_RAMP_PER_TURN`). */
export const JUICE_RAMP_PER_TURN = 1
/** Inclusive Heat bounds; no state may leave them (`HEAT_BOUNDS`). */
export const HEAT_MIN = 0
export const HEAT_MAX = 10

// ── Seats & configuration ──────────────────────────────────────────────────────

/** Which of the two seats a value refers to (mirrors `Player`). */
export type Seat = 'A' | 'B'

/** The other seat. */
export function opponent(seat: Seat): Seat {
  return seat === 'A' ? 'B' : 'A'
}

/**
 * A player's opening Outfit (mirrors `OutfitConfig`): the board it brings, its
 * deck, its Boss, and its opening resource counters. A legal opening sits within
 * every cap; {@link defaultOutfit} returns one, matching `OutfitConfig::new`.
 */
export interface OutfitConfig {
  readonly name: string
  readonly bossName: string
  readonly bossHp: number
  readonly operators: number
  readonly vehicles: number
  readonly deckSize: number
  readonly startingHeat: number
  readonly startingJuice: number
  readonly availableJuice: number
  readonly heistResolved: boolean
  readonly outstandingHeistPrereqs: number
}

/** A legal opening Outfit named `name` (mirrors `OutfitConfig::new`). */
export function defaultOutfit(name: string): OutfitConfig {
  return {
    name,
    bossName: `${name}-boss`,
    bossHp: 30,
    operators: 2,
    vehicles: 1,
    deckSize: 30,
    startingHeat: 0,
    startingJuice: STARTING_JUICE,
    availableJuice: 3,
    heistResolved: false,
    outstandingHeistPrereqs: 0,
  }
}

// ── Live match state ───────────────────────────────────────────────────────────

/** The live, mutable-through-events state of one seat during a match. */
export interface SeatState {
  readonly outfit: string
  readonly bossName: string
  readonly bossHp: number
  readonly heat: number
  /** Currently available Juice pool (ramps each of the seat's turns). */
  readonly juice: number
  readonly operators: number
  readonly vehicles: number
  readonly deckSize: number
  readonly heistResolved: boolean
  readonly outstandingHeistPrereqs: number
}

/** Lifecycle of a match as the client observes it. */
export type MatchPhase = 'idle' | 'active' | 'completed'

/** The full board/game state the Canvas renders and the rules fold events into. */
export interface MatchState {
  readonly matchId: string
  readonly seats: Readonly<Record<Seat, SeatState>>
  /** Whose turn it is, or `null` before the match opens / after it ends. */
  readonly turn: Seat | null
  readonly phase: MatchPhase
  /** The winning seat once the match is completed, else `null`. */
  readonly winner: Seat | null
  /** Deterministic RNG seed the match opened with (0 before start). */
  readonly rngSeed: number
}

/** Build a `SeatState` from an opening Outfit. */
export function seatFromOutfit(outfit: OutfitConfig): SeatState {
  return {
    outfit: outfit.name,
    bossName: outfit.bossName,
    bossHp: outfit.bossHp,
    heat: outfit.startingHeat,
    juice: outfit.availableJuice,
    operators: outfit.operators,
    vehicles: outfit.vehicles,
    deckSize: outfit.deckSize,
    heistResolved: outfit.heistResolved,
    outstandingHeistPrereqs: outfit.outstandingHeistPrereqs,
  }
}

// ── Player actions (mirror the command payloads) ───────────────────────────────

/**
 * A player intent, expressed against a seat. Each variant maps to a
 * `crates/game-session` command; the `kind` is the wire command name so the
 * connection can forward it to the authoritative server verbatim.
 */
export type MatchAction =
  | { readonly kind: 'PlayCardCmd'; readonly seat: Seat; readonly cardInstanceId: string; readonly targetRef: string; readonly juiceCost: number }
  | { readonly kind: 'ActivateHeroPowerCmd'; readonly seat: Seat; readonly targetRef: string; readonly juiceCost: number }
  | { readonly kind: 'EndTurnCmd'; readonly seat: Seat }
  | { readonly kind: 'ConcedeMatchCmd'; readonly seat: Seat }

/** The wire command name of an action (what the authoritative server parses). */
export type CommandName = MatchAction['kind']

// ── Authoritative deltas (mirror the emitted `Event`s) ─────────────────────────

/**
 * One authoritative state delta, mirroring a `game-session` `Event`. The server
 * pushes these (individually or batched in a {@link MatchDelta}); the client
 * folds them into {@link MatchState}. The `type` values are the crate's
 * `event_type()` strings so the two ends agree on the vocabulary.
 */
export type DeltaEvent =
  | { readonly type: 'match.started'; readonly matchId: string; readonly playerAOutfit: string; readonly playerBOutfit: string; readonly rngSeed: number; readonly openingPlayer: Seat }
  | { readonly type: 'card.played'; readonly player: Seat; readonly cardInstanceId: string; readonly targetRef: string; readonly juiceSpent: number }
  | { readonly type: 'heat.raised'; readonly player: Seat; readonly amount: number; readonly newHeat: number }
  | { readonly type: 'hero_power.activated'; readonly player: Seat; readonly targetRef: string; readonly juiceSpent: number; readonly remainingJuice: number }
  | { readonly type: 'turn.ended'; readonly player: Seat; readonly nextPlayer: Seat; readonly nextPlayerJuice: number }
  | { readonly type: 'match.completed'; readonly concedingPlayer: Seat; readonly winner: Seat }

/** The `type()` string of a delta event (mirrors `event_type()`). */
export type DeltaEventType = DeltaEvent['type']

/**
 * An inbound WebSocket frame from the authoritative game server. The protocol is
 * tolerant of the current scaffold server (which replies with a bare `ok` /
 * error-string *ack* per command) and of the richer delta/snapshot envelope a
 * fuller server will push:
 *
 *  - `snapshot` — a full authoritative `MatchState` (join / resync),
 *  - `delta`    — one or more authoritative events to fold in,
 *  - `ack`      — accept/reject of the client's most recent predicted action.
 */
export type ServerMessage =
  | { readonly type: 'snapshot'; readonly state: MatchState }
  | { readonly type: 'delta'; readonly events: readonly DeltaEvent[] }
  | { readonly type: 'ack'; readonly accepted: boolean; readonly reason?: string }

/** Clamp `n` into `[min, max]` (Juice/Heat never leave their bounds). */
export function clamp(n: number, min: number, max: number): number {
  return n < min ? min : n > max ? max : n
}
