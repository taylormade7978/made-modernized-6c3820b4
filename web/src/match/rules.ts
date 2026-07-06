/**
 * The local rules engine: a faithful TypeScript mirror of the authoritative
 * `crates/game-session` aggregate, used for two things the story requires:
 *
 *  1. **Optimistic UI** — {@link validateAction} rejects an illegal move *before*
 *     it is sent, and {@link applyAction} predicts the resulting deltas so the
 *     board updates instantly; the server's authoritative deltas then reconcile.
 *  2. **Offline practice** — with no server at all, {@link applyAction} runs a
 *     whole match client-side against the same invariants.
 *
 * It is deliberately a *pure* module (no DOM, no network): every function takes
 * a state and returns a new state / a decision, so it is unit-testable and can
 * back both the optimistic layer and practice mode unchanged.
 *
 * Where the crate consults server-only knowledge (hidden deck order, the seeded
 * RNG) the mirror cannot — and must not — guess; those outcomes arrive as
 * authoritative deltas. The invariants mirrored here are the ones a client can
 * check from visible state: turn ownership, Juice affordability, board caps, and
 * the Heat/Juice bounds.
 */
import {
  clamp,
  defaultOutfit,
  HEAT_MAX,
  HEAT_MIN,
  HEAT_PER_PLAY,
  JUICE_CAP,
  JUICE_RAMP_PER_TURN,
  opponent,
  seatFromOutfit,
  type DeltaEvent,
  type MatchAction,
  type MatchState,
  type OutfitConfig,
  type Seat,
  type SeatState,
} from './model'

/** Outcome of validating an action against a state: legal, or a stated reason. */
export type Validation = { readonly ok: true } | { readonly ok: false; readonly reason: string }

const OK: Validation = { ok: true }
function reject(reason: string): Validation {
  return { ok: false, reason }
}

/** Open a fresh match between two Outfits with `A` to move (mirrors `StartMatch`). */
export function startMatch(
  matchId: string,
  playerA: OutfitConfig = defaultOutfit(`${matchId}-a`),
  playerB: OutfitConfig = defaultOutfit(`${matchId}-b`),
  rngSeed = 0xc0ffee,
): MatchState {
  return {
    matchId,
    seats: { A: seatFromOutfit(playerA), B: seatFromOutfit(playerB) },
    turn: 'A',
    phase: 'active',
    winner: null,
    rngSeed,
  }
}

/**
 * Decide whether `action` is legal against `state` — the same gate the server
 * applies, restricted to what visible state can prove. A rejection here means
 * the move is *never sent*; a move that passes here may still be corrected by an
 * authoritative delta (mispredicted server-only knowledge), which the optimistic
 * layer rolls back.
 */
export function validateAction(state: MatchState, action: MatchAction): Validation {
  if (state.phase !== 'active') {
    return reject('the match is not in progress')
  }

  const seat = state.seats[action.seat]

  // Concede is the one command exempt from the whose-turn-it-is rule.
  if (action.kind !== 'ConcedeMatchCmd' && state.turn !== action.seat) {
    return reject('it is not your turn')
  }

  switch (action.kind) {
    case 'PlayCardCmd': {
      if (!action.cardInstanceId.trim()) return reject('no card selected')
      if (!action.targetRef.trim()) return reject('no target selected')
      if (action.juiceCost > seat.juice) {
        return reject(`not enough Juice (need ${action.juiceCost}, have ${seat.juice})`)
      }
      // Playing a card always raises Heat; the raise may not leave the bounds.
      if (seat.heat + HEAT_PER_PLAY > HEAT_MAX) {
        return reject('too much Heat — a Cop Event must resolve first')
      }
      return OK
    }
    case 'ActivateHeroPowerCmd': {
      if (!action.targetRef.trim()) return reject('no target selected')
      if (action.juiceCost > seat.juice) {
        return reject(`not enough Juice (need ${action.juiceCost}, have ${seat.juice})`)
      }
      return OK
    }
    case 'EndTurnCmd':
      return OK
    case 'ConcedeMatchCmd':
      return OK
  }
}

/**
 * Apply a *validated* action, returning the new state and the authoritative-shaped
 * deltas it predicts. The events use the same `type` vocabulary the server emits,
 * so the optimistic prediction and a real server delta fold through the identical
 * {@link foldEvent} path — the property that makes reconciliation trustworthy.
 *
 * Throws if `action` is not legal for `state`; callers validate first (the
 * optimistic layer and practice loop both do).
 */
export function applyAction(state: MatchState, action: MatchAction): { state: MatchState; events: DeltaEvent[] } {
  const decision = validateAction(state, action)
  if (!decision.ok) {
    throw new Error(`illegal action ${action.kind}: ${decision.reason}`)
  }

  const seat = state.seats[action.seat]

  switch (action.kind) {
    case 'PlayCardCmd': {
      const newHeat = clamp(seat.heat + HEAT_PER_PLAY, HEAT_MIN, HEAT_MAX)
      const events: DeltaEvent[] = [
        {
          type: 'card.played',
          player: action.seat,
          cardInstanceId: action.cardInstanceId,
          targetRef: action.targetRef,
          juiceSpent: action.juiceCost,
        },
        { type: 'heat.raised', player: action.seat, amount: HEAT_PER_PLAY, newHeat },
      ]
      return { state: foldEvents(state, events), events }
    }
    case 'ActivateHeroPowerCmd': {
      const remaining = clamp(seat.juice - action.juiceCost, 0, JUICE_CAP)
      const events: DeltaEvent[] = [
        {
          type: 'hero_power.activated',
          player: action.seat,
          targetRef: action.targetRef,
          juiceSpent: action.juiceCost,
          remainingJuice: remaining,
        },
      ]
      return { state: foldEvents(state, events), events }
    }
    case 'EndTurnCmd': {
      const next = opponent(action.seat)
      const nextJuice = clamp(state.seats[next].juice + JUICE_RAMP_PER_TURN, 0, JUICE_CAP)
      const events: DeltaEvent[] = [
        { type: 'turn.ended', player: action.seat, nextPlayer: next, nextPlayerJuice: nextJuice },
      ]
      return { state: foldEvents(state, events), events }
    }
    case 'ConcedeMatchCmd': {
      const events: DeltaEvent[] = [
        { type: 'match.completed', concedingPlayer: action.seat, winner: opponent(action.seat) },
      ]
      return { state: foldEvents(state, events), events }
    }
  }
}

/** Fold a batch of authoritative deltas into a state, in order. */
export function foldEvents(state: MatchState, events: readonly DeltaEvent[]): MatchState {
  return events.reduce(foldEvent, state)
}

/**
 * Fold a single authoritative delta into `state`, returning the next state. This
 * is the one place the board mutates: both the server's authoritative deltas and
 * the optimistic prediction flow through here, so they can never drift in how a
 * given event changes the board.
 */
export function foldEvent(state: MatchState, event: DeltaEvent): MatchState {
  switch (event.type) {
    case 'match.started':
      return {
        ...state,
        matchId: event.matchId,
        rngSeed: event.rngSeed,
        turn: event.openingPlayer,
        phase: 'active',
        winner: null,
      }
    case 'card.played':
      return patchSeat(state, event.player, (s) => ({
        ...s,
        juice: clamp(s.juice - event.juiceSpent, 0, JUICE_CAP),
      }))
    case 'heat.raised':
      return patchSeat(state, event.player, (s) => ({ ...s, heat: event.newHeat }))
    case 'hero_power.activated':
      return patchSeat(state, event.player, (s) => ({ ...s, juice: event.remainingJuice }))
    case 'turn.ended':
      return {
        ...patchSeat(state, event.nextPlayer, (s) => ({ ...s, juice: event.nextPlayerJuice })),
        turn: event.nextPlayer,
      }
    case 'match.completed':
      return { ...state, phase: 'completed', winner: event.winner, turn: null }
  }
}

/** Return a copy of `state` with `seat`'s `SeatState` transformed by `patch`. */
function patchSeat(state: MatchState, seat: Seat, patch: (s: SeatState) => SeatState): MatchState {
  return { ...state, seats: { ...state.seats, [seat]: patch(state.seats[seat]) } }
}
