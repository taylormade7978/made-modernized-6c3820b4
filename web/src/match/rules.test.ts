/**
 * Local rules-engine tests.
 *
 * These pin the invariants the browser mirrors from `crates/game-session`:
 * turn ownership, Juice affordability, the Heat cap / Cop Event, card effects
 * (damage / summon / attack / win), and that a prediction and an authoritative
 * delta fold through the same event path. Plain TS against pure functions.
 */
import { describe, expect, it } from 'vitest'
import { aiTurn, applyAction, foldEvent, startMatch, validateAction } from './rules'
import { JUICE_CAP, opponent, type HandCard, type MatchAction, type MatchState, type Seat } from './model'

const card = (over: Partial<HandCard>): HandCard => ({
  cardId: 'bolt', name: 'Bolt', cost: 1, type: 'Job', effect: 'damage', amount: 3, instanceId: 'inst-1', ...over,
})
/** Give `seat` a controlled hand + plenty of Juice for deterministic tests. */
const withHand = (s: MatchState, seat: Seat, hand: HandCard[], juice = 10): MatchState => ({
  ...s,
  seats: { ...s.seats, [seat]: { ...s.seats[seat], hand, juice } },
})
const playOf = (seat: Seat, c: HandCard): MatchAction => ({
  kind: 'PlayCardCmd', seat, cardInstanceId: c.instanceId, targetRef: `boss:${opponent(seat)}`, juiceCost: c.cost,
})
/** Mark all of `seat`'s Operators ready (clear summoning sickness). */
const ready = (s: MatchState, seat: Seat): MatchState => ({
  ...s,
  seats: { ...s.seats, [seat]: { ...s.seats[seat], board: s.seats[seat].board.map((u) => ({ ...u, ready: true })) } },
})

describe('validateAction', () => {
  it('accepts a legal play by the turn-holder', () => {
    const c = card({ instanceId: 'a1' })
    const s = withHand(startMatch('m'), 'A', [c])
    expect(validateAction(s, playOf('A', c)).ok).toBe(true)
  })

  it('rejects an action from the seat that does not hold the turn', () => {
    const c = card({ instanceId: 'b1' })
    const s = withHand(startMatch('m'), 'B', [c])
    expect(validateAction(s, playOf('B', c))).toEqual({ ok: false, reason: 'it is not your turn' })
  })

  it('rejects a play the seat cannot afford', () => {
    const c = card({ instanceId: 'a1', cost: 6 })
    const s = withHand(startMatch('m'), 'A', [c], 3)
    expect(validateAction(s, playOf('A', c)).ok).toBe(false)
  })

  it('allows concede off-turn (the one exemption from the turn rule)', () => {
    expect(validateAction(startMatch('m'), { kind: 'ConcedeMatchCmd', seat: 'B' }).ok).toBe(true)
  })

  it('rejects any action once the match is completed', () => {
    const c = card({ instanceId: 'a1' })
    const s = { ...withHand(startMatch('m'), 'A', [c]), phase: 'completed' as const }
    expect(validateAction(s, playOf('A', c)).ok).toBe(false)
  })
})

describe('applyAction', () => {
  it('spends Juice, raises Heat, removes the card, and damages the enemy boss', () => {
    const c = card({ instanceId: 'a1', cost: 2, effect: 'damage', amount: 3 })
    const s0 = withHand(startMatch('m'), 'A', [c])
    const { state, events } = applyAction(s0, playOf('A', c))
    expect(events.map((e) => e.type)).toEqual(['card.played', 'heat.raised', 'boss.damaged'])
    expect(state.seats.A.juice).toBe(8) // 10 - 2
    expect(state.seats.A.heat).toBe(1)
    expect(state.seats.A.hand).toHaveLength(0)
    expect(state.seats.B.bossHp).toBe(27) // 30 - 3
  })

  it('summons an Operator that can attack from the next turn', () => {
    const c = card({ instanceId: 'a1', cost: 1, effect: 'summon', amount: 0, atk: 4, hp: 3 })
    const s = applyAction(withHand(startMatch('m'), 'A', [c]), playOf('A', c)).state
    expect(s.seats.A.board).toHaveLength(1)
    expect(s.seats.A.board[0].ready).toBe(false) // summoning sickness
  })

  it('an Operator can attack the enemy boss and then exhausts', () => {
    const c = card({ instanceId: 'a1', cost: 1, effect: 'summon', amount: 0, atk: 5, hp: 5 })
    let s = applyAction(withHand(startMatch('m'), 'A', [c]), playOf('A', c)).state
    s = ready(s, 'A')
    const { state } = applyAction(s, { kind: 'AttackCmd', seat: 'A', attackerId: 'a1', targetRef: 'boss:B' })
    expect(state.seats.B.bossHp).toBe(25) // 30 - 5
    expect(state.seats.A.board[0].ready).toBe(false)
  })

  it('operators trade damage in combat and the loser dies', () => {
    // A's 3/2 attacks B's 2/2: B's unit takes 3 (dies), A's unit takes 2 back (dies).
    let s = startMatch('m')
    s = {
      ...s,
      seats: {
        A: { ...s.seats.A, board: [{ instanceId: 'a1', name: 'Homie', cardId: 'x', atk: 3, hp: 2, maxHp: 2, ready: true, keywords: [] }] },
        B: { ...s.seats.B, board: [{ instanceId: 'b1', name: 'Buck', cardId: 'y', atk: 2, hp: 2, maxHp: 2, ready: true, keywords: [] }] },
      },
    }
    const { state } = applyAction(s, { kind: 'AttackCmd', seat: 'A', attackerId: 'a1', targetRef: 'op:b1' })
    expect(state.seats.B.board).toHaveLength(0) // B's 2-hp unit took 3, died
    expect(state.seats.A.board).toHaveLength(0) // A's 2-hp unit took 2 back, died
  })

  it('Spotlight forces attacks onto the taunting Operator', () => {
    let s = startMatch('m')
    s = {
      ...s,
      seats: {
        A: { ...s.seats.A, board: [{ instanceId: 'a1', name: 'Att', cardId: 'x', atk: 2, hp: 3, maxHp: 3, ready: true, keywords: [] }] },
        B: { ...s.seats.B, board: [{ instanceId: 'guard', name: 'Riot Squad', cardId: 'y', atk: 4, hp: 5, maxHp: 5, ready: false, keywords: ['Spotlight'] }] },
      },
    }
    // Going face is illegal while the Spotlight guard stands.
    expect(validateAction(s, { kind: 'AttackCmd', seat: 'A', attackerId: 'a1', targetRef: 'boss:B' }).ok).toBe(false)
    // Attacking the guard is legal.
    expect(validateAction(s, { kind: 'AttackCmd', seat: 'A', attackerId: 'a1', targetRef: 'op:guard' }).ok).toBe(true)
  })

  it('a targeted damage spell can kill an enemy Operator', () => {
    const c = card({ instanceId: 'a1', cost: 2, effect: 'damage', amount: 4 })
    let s = withHand(startMatch('m'), 'A', [c])
    s = { ...s, seats: { ...s.seats, B: { ...s.seats.B, board: [{ instanceId: 'b1', name: 'Buck', cardId: 'y', atk: 2, hp: 3, maxHp: 3, ready: false, keywords: [] }] } } }
    const { state } = applyAction(s, { kind: 'PlayCardCmd', seat: 'A', cardInstanceId: 'a1', targetRef: 'op:b1', juiceCost: 2 })
    expect(state.seats.B.board).toHaveLength(0) // 3 hp, took 4, dead
  })

  it('completes the match when a boss reaches 0', () => {
    const c = card({ instanceId: 'a1', cost: 5, effect: 'damage', amount: 7 })
    const s0 = withHand({ ...startMatch('m'), seats: { ...startMatch('m').seats, B: { ...startMatch('m').seats.B, bossHp: 5 } } }, 'A', [c])
    const { state } = applyAction(s0, playOf('A', c))
    expect(state.phase).toBe('completed')
    expect(state.winner).toBe('A')
  })

  it('fires a Cop Event that cools Heat and raids the hottest player', () => {
    const c = card({ instanceId: 'a1', cost: 1, effect: 'damage', amount: 1 })
    const s0 = withHand({ ...startMatch('m'), seats: { ...startMatch('m').seats, A: { ...startMatch('m').seats.A, heat: 9 } } }, 'A', [c])
    const { state, events } = applyAction(s0, playOf('A', c))
    expect(events.some((e) => e.type === 'cop.raided')).toBe(true)
    expect(state.seats.A.heat).toBe(3) // reset
    expect(state.seats.A.bossHp).toBe(27) // 30 - 3 cop damage
  })

  it('passes the turn, readies operators, draws, and ramps (capped)', () => {
    const s0 = { ...startMatch('m'), seats: { ...startMatch('m').seats, B: { ...startMatch('m').seats.B, juice: JUICE_CAP } } }
    const { state } = applyAction(s0, { kind: 'EndTurnCmd', seat: 'A' })
    expect(state.turn).toBe('B')
    expect(state.seats.B.juice).toBe(JUICE_CAP) // already capped, no overflow
    expect(state.seats.B.hand.length).toBe(5) // 4 opening + 1 drawn
  })

  it('completes the match with the opponent as winner on concede', () => {
    const { state } = applyAction(startMatch('m'), { kind: 'ConcedeMatchCmd', seat: 'A' })
    expect(state.phase).toBe('completed')
    expect(state.winner).toBe('B')
    expect(state.turn).toBeNull()
  })

  it('throws when asked to apply an illegal action', () => {
    expect(() => applyAction(startMatch('m'), playOf('B', card({ instanceId: 'x' })))).toThrow()
  })
})

describe('aiTurn', () => {
  it('takes a full turn and passes back to the opponent', () => {
    const s0 = { ...startMatch('m'), turn: 'B' as const }
    const { state } = aiTurn(s0, 'B')
    // Either it ended its turn (turn back to A) or it won outright.
    expect(state.turn === 'A' || state.phase === 'completed').toBe(true)
  })
})

describe('foldEvent', () => {
  it('folds an authoritative turn.ended delta identically to a prediction', () => {
    const folded = foldEvent(startMatch('m'), { type: 'turn.ended', player: 'A', nextPlayer: 'B', nextPlayerJuice: 4, nextPlayerMaxJuice: 4 })
    expect(folded.turn).toBe('B')
    expect(folded.seats.B.juice).toBe(4)
    // The grown crystal folds onto the incoming seat's max-Juice, not just its pool.
    expect(folded.seats.B.maxJuice).toBe(4)
  })
})
