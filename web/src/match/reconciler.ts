/**
 * Client-side prediction & reconciliation — the heart of the optimistic UI.
 *
 * The engine keeps two things: the last **confirmed** state (authoritative
 * truth, as of the most recent server delta/ack) and a FIFO queue of **pending**
 * predicted actions the client has applied locally but the server has not yet
 * confirmed. The {@link Reconciler.view} the Canvas renders is the confirmed
 * state with every pending prediction folded on top, so a legal move shows
 * instantly.
 *
 * Reconciliation keeps a hard invariant: the pending queue is *always* a legal
 * sequence from the confirmed state. So whenever the ground shifts under it —
 * the server rejects a prediction, or pushes an authoritative delta that
 * invalidates one — {@link Reconciler} re-validates the queue and drops any
 * prediction that is no longer legal. Dropping a prediction *is* the rollback:
 * the next {@link Reconciler.view} no longer includes its effect, and the board
 * visibly snaps back to server truth.
 *
 * It is transport-agnostic and pure (no DOM, no socket): `connection.ts` wires
 * WebSocket frames to these methods, and `useMatch` renders `view()`. That
 * separation is what makes the rollback logic unit-testable without a browser.
 */
import { applyAction, foldEvents, validateAction } from './rules'
import type { DeltaEvent, MatchAction, MatchState } from './model'

/** A predicted action applied locally and awaiting the server's verdict. */
export interface Pending {
  /** Monotonic client id, for correlating a UI intent to its outcome. */
  readonly id: string
  readonly action: MatchAction
}

/** Result of predicting an action: accepted locally (with its id), or refused. */
export type PredictResult =
  | { readonly ok: true; readonly id: string }
  | { readonly ok: false; readonly reason: string }

/** What a reconciliation step rolled back, for surfacing a correction to the UI. */
export interface Correction {
  /** The prediction the server explicitly rejected (rejectHead only). */
  readonly rolledBack: Pending | null
  /** Downstream predictions dropped because they were no longer legal. */
  readonly dropped: readonly Pending[]
}

export class Reconciler {
  private confirmed: MatchState
  private pending: Pending[] = []
  private seq = 0

  constructor(initial: MatchState) {
    this.confirmed = initial
  }

  /** The last authoritative state (no pending predictions folded in). */
  confirmedState(): MatchState {
    return this.confirmed
  }

  /** The optimistic state to render: confirmed + every pending prediction. */
  view(): MatchState {
    let s = this.confirmed
    for (const p of this.pending) s = applyAction(s, p.action).state
    return s
  }

  /** How many predictions are in flight (awaiting a server verdict). */
  pendingCount(): number {
    return this.pending.length
  }

  /**
   * Validate `action` against the current optimistic view and, if legal, apply
   * it as a pending prediction. An illegal action is refused here and never
   * enters the queue (nor gets sent) — the first line of the optimistic UI.
   */
  predict(action: MatchAction): PredictResult {
    const decision = validateAction(this.view(), action)
    if (!decision.ok) return { ok: false, reason: decision.reason }
    const id = `p${(this.seq += 1)}`
    this.pending.push({ id, action })
    return { ok: true, id }
  }

  /**
   * The server accepted the oldest in-flight prediction: promote it into the
   * confirmed state. Ack correlation is FIFO — the scaffold server replies with a
   * bare `ok` per command, so the oldest unconfirmed prediction is the one being
   * answered. Returns the promoted prediction, or `null` if none was pending.
   */
  confirmHead(): Pending | null {
    const head = this.pending.shift()
    if (!head) return null
    // `head` is legal against `confirmed` by the queue invariant, so this holds.
    this.confirmed = applyAction(this.confirmed, head.action).state
    return head
  }

  /**
   * The server rejected the oldest in-flight prediction (an illegal / mispredicted
   * move): discard it *without* touching confirmed state, then re-validate the
   * rest of the queue — a downstream prediction that depended on the rejected one
   * may now be illegal and is dropped too. The returned {@link Correction} drives
   * the visible rollback banner.
   */
  rejectHead(): Correction {
    const rolledBack = this.pending.shift() ?? null
    const dropped = this.rebuild()
    return { rolledBack, dropped }
  }

  /**
   * Fold authoritative deltas the server pushed (independent of a specific
   * prediction) into confirmed state, then re-validate the pending queue against
   * the new truth. Predictions the delta invalidated are dropped (rolled back).
   */
  applyAuthoritative(events: readonly DeltaEvent[]): Correction {
    this.confirmed = foldEvents(this.confirmed, events)
    return { rolledBack: null, dropped: this.rebuild() }
  }

  /** Replace confirmed state wholesale (a server snapshot / resync) and rebuild. */
  reset(state: MatchState): Correction {
    this.confirmed = state
    return { rolledBack: null, dropped: this.rebuild() }
  }

  /**
   * Re-derive the pending queue against the current confirmed state, keeping the
   * longest legal prefix-in-sequence and dropping any prediction that no longer
   * validates. Restores the "pending queue is always legal from confirmed"
   * invariant after confirmed moves underneath it.
   */
  private rebuild(): Pending[] {
    const kept: Pending[] = []
    const dropped: Pending[] = []
    let s = this.confirmed
    for (const p of this.pending) {
      const decision = validateAction(s, p.action)
      if (decision.ok) {
        s = applyAction(s, p.action).state
        kept.push(p)
      } else {
        dropped.push(p)
      }
    }
    this.pending = kept
    return dropped
  }
}
