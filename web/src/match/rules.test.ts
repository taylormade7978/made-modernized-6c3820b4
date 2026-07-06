/**
 * Local rules-engine tests.
 *
 * These pin the invariants the browser mirrors from `crates/game-session`:
 * turn ownership, Juice affordability, the Heat cap, and that both an optimistic
 * prediction and an authoritative delta fold through the same event path. They
 * are plain TS against pure functions (no DOM), matching the API client tests.
 */
import { describe, expect, it } from 'vitest'
import { applyAction, foldEvent, startMatch, validateAction } from './rules'
import { JUICE_CAP, type MatchAction } from './model'

const play = (seat: 'A' | 'B', juiceCost: number): MatchAction => ({
  kind: 'PlayCardCmd',
  seat,
  cardInstanceId: 'card-1',
  targetRef: 'boss:B',
  juiceCost,
})

describe('validateAction', () => {
  it('accepts a legal play by the turn-holder', () => {
    const s = startMatch('m')
    expect(validateAction(s, play('A', 1)).ok).toBe(true)
  })

  it('rejects an action from the seat that does not hold the turn', () => {
    const s = startMatch('m') // A to move
    const decision = validateAction(s, play('B', 1))
    expect(decision).toEqual({ ok: false, reason: 'it is not your turn' })
  })

  it('rejects a play the seat cannot afford', () => {
    const s = startMatch('m') // A opens with availableJuice 3
    const decision = validateAction(s, play('A', 4))
    expect(decision.ok).toBe(false)
  })

  it('allows concede off-turn (the one exemption from the turn rule)', () => {
    const s = startMatch('m') // A to move
    expect(validateAction(s, { kind: 'ConcedeMatchCmd', seat: 'B' }).ok).toBe(true)
  })

  it('rejects any action once the match is completed', () => {
    const s = { ...startMatch('m'), phase: 'completed' as const }
    expect(validateAction(s, play('A', 1)).ok).toBe(false)
  })
})

describe('applyAction', () => {
  it('spends Juice and raises Heat when a card is played', () => {
    const s0 = startMatch('m')
    const { state, events } = applyAction(s0, play('A', 2))
    expect(events.map((e) => e.type)).toEqual(['card.played', 'heat.raised'])
    expect(state.seats.A.juice).toBe(1) // 3 - 2
    expect(state.seats.A.heat).toBe(1) // +HEAT_PER_PLAY
  })

  it('passes the turn and ramps the incoming seat, capped at JUICE_CAP', () => {
    const s0 = { ...startMatch('m'), seats: { ...startMatch('m').seats, B: { ...startMatch('m').seats.B, juice: JUICE_CAP } } }
    const { state } = applyAction(s0, { kind: 'EndTurnCmd', seat: 'A' })
    expect(state.turn).toBe('B')
    expect(state.seats.B.juice).toBe(JUICE_CAP) // already capped, does not overflow
  })

  it('completes the match with the opponent as winner on concede', () => {
    const { state } = applyAction(startMatch('m'), { kind: 'ConcedeMatchCmd', seat: 'A' })
    expect(state.phase).toBe('completed')
    expect(state.winner).toBe('B')
    expect(state.turn).toBeNull()
  })

  it('throws when asked to apply an illegal action', () => {
    expect(() => applyAction(startMatch('m'), play('B', 1))).toThrow()
  })
})

describe('foldEvent', () => {
  it('folds an authoritative turn.ended delta identically to a prediction', () => {
    const s0 = startMatch('m')
    const folded = foldEvent(s0, { type: 'turn.ended', player: 'A', nextPlayer: 'B', nextPlayerJuice: 4 })
    expect(folded.turn).toBe('B')
    expect(folded.seats.B.juice).toBe(4)
  })
})
