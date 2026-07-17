/**
 * Reconciler tests — the optimistic prediction / rollback core.
 *
 * They exercise the three ways confirmed truth can move under the pending queue:
 * a FIFO confirm, an explicit reject (visible rollback), and an authoritative
 * delta that invalidates an in-flight prediction (server-correction rollback).
 * Transport-agnostic and DOM-free, so the rollback logic is verified without a
 * browser or a socket.
 */
import { describe, expect, it } from 'vitest'
import { Reconciler } from './reconciler'
import { startMatch } from './rules'
import type { MatchAction, MatchState } from './model'

// A fixed opening where A holds a known cost-2 card `c1` and 3 Juice, so the
// spend assertions below are deterministic regardless of the dealt shuffle.
const opening = (): MatchState => {
  const s = startMatch('m')
  return {
    ...s,
    seats: {
      ...s.seats,
      A: { ...s.seats.A, juice: 3, hand: [{ cardId: 'bolt', name: 'Bolt', cost: 2, type: 'Job', effect: 'damage', amount: 3, instanceId: 'c1' }] },
    },
  }
}
const play: MatchAction = { kind: 'PlayCardCmd', seat: 'A', cardInstanceId: 'c1', targetRef: 'boss:B', juiceCost: 2 }
const endTurn: MatchAction = { kind: 'EndTurnCmd', seat: 'A' }

describe('Reconciler', () => {
  it('applies a prediction to the view but not to confirmed until acked', () => {
    const r = new Reconciler(opening())
    const result = r.predict(play)
    expect(result.ok).toBe(true)
    expect(r.pendingCount()).toBe(1)
    // Optimistic view reflects the spend; confirmed truth does not yet.
    expect(r.view().seats.A.juice).toBe(1)
    expect(r.confirmedState().seats.A.juice).toBe(3)
  })

  it('refuses an illegal prediction without queuing it', () => {
    const r = new Reconciler(opening())
    const result = r.predict({ ...play, cardInstanceId: 'not-in-hand' })
    expect(result.ok).toBe(false)
    expect(r.pendingCount()).toBe(0)
  })

  it('promotes the oldest prediction on a FIFO confirm', () => {
    const r = new Reconciler(opening())
    r.predict(play)
    r.predict(endTurn)
    const promoted = r.confirmHead()
    expect(promoted?.action).toEqual(play)
    expect(r.confirmedState().seats.A.juice).toBe(1) // play is now truth
    expect(r.pendingCount()).toBe(1) // endTurn still pending
  })

  it('rolls back the rejected prediction, leaving confirmed untouched', () => {
    const r = new Reconciler(opening())
    r.predict(play)
    expect(r.view().seats.A.juice).toBe(1)
    const correction = r.rejectHead()
    expect(correction.rolledBack?.action).toEqual(play)
    // Rollback: the view snaps back to confirmed truth.
    expect(r.view().seats.A.juice).toBe(3)
    expect(r.pendingCount()).toBe(0)
  })

  it('drops a prediction an authoritative delta invalidated (server correction)', () => {
    const r = new Reconciler(opening())
    r.predict(play) // A predicts a card while it is A's turn
    // Server truth: A's turn actually ended → A no longer holds the turn.
    const correction = r.applyAuthoritative([
      { type: 'turn.ended', player: 'A', nextPlayer: 'B', nextPlayerJuice: 4, nextPlayerMaxJuice: 4 },
    ])
    expect(correction.dropped).toHaveLength(1)
    expect(r.view().turn).toBe('B')
    // The mispredicted spend was rolled back to server truth (juice 3).
    expect(r.view().seats.A.juice).toBe(3)
  })
})
