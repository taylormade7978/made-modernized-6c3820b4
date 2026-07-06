/**
 * `useMatch` — the React controller that binds the match subsystem together.
 *
 * It owns a {@link Reconciler} (prediction + rollback), optionally a
 * {@link MatchConnection} (the authoritative socket), and a {@link BoardRenderer}
 * drawing to a `<canvas>` on an animation-frame loop. The view consumes the
 * returned {@link MatchController}: the current (optimistic) board, connection
 * status, and a small set of player actions.
 *
 * Two modes share one code path:
 *  - **online** — a predicted action is applied locally *and* sent to the server;
 *    the server's ack/delta confirms it or rolls it back (visible reconciliation);
 *  - **practice** — no socket at all; the local rules WASM/mirror is the sole
 *    authority, so a predicted action is confirmed immediately and a whole match
 *    runs fully client-side.
 *
 * The optimistic gate is two-stage: an action is first checked against the
 * loaded rules WASM name-gate (when the compiled crate is present), then against
 * the {@link Reconciler}'s local validation, before it is ever sent.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { MatchConnection, type ConnectionStatus } from './connection'
import { Reconciler } from './reconciler'
import { BoardRenderer } from './renderer'
import { startMatch } from './rules'
import { loadRulesWasm, type RulesWasm } from './wasm'
import type { CommandName, MatchAction, MatchState, Seat } from './model'

/** How the view is playing: against the server, or fully offline. */
export type MatchMode = 'online' | 'practice'

/** The seat the local player controls. */
const SELF_SEAT: Seat = 'A'

/** Everything the {@link MatchView} needs to render and drive a match. */
export interface MatchController {
  readonly state: MatchState
  readonly mode: MatchMode
  readonly selfSeat: Seat
  readonly status: ConnectionStatus | 'practice'
  readonly pending: number
  /** Reason for the most recent rollback, or `null` (drives the banner). */
  readonly correction: string | null
  readonly canvasRef: React.RefObject<HTMLCanvasElement>
  readonly playCard: () => void
  readonly heroPower: () => void
  readonly endTurn: () => void
  readonly concede: () => void
  readonly setMode: (mode: MatchMode) => void
  readonly newMatch: () => void
  readonly dismissCorrection: () => void
}

export function useMatch(matchId = 'live'): MatchController {
  const reconciler = useRef<Reconciler>(new Reconciler(startMatch(matchId)))
  const connection = useRef<MatchConnection | null>(null)
  const wasmGate = useRef<RulesWasm | null>(null)
  const cardSeq = useRef(0)
  const flash = useRef(0)

  const canvasRef = useRef<HTMLCanvasElement>(null)
  const rendererRef = useRef<BoardRenderer | null>(null)

  const [mode, setModeState] = useState<MatchMode>('online')
  const [status, setStatus] = useState<ConnectionStatus | 'practice'>('connecting')
  const [correction, setCorrection] = useState<string | null>(null)
  // Bumped whenever the board changes, to re-render the React chrome (the canvas
  // itself redraws on its own rAF loop from refs).
  const [, bump] = useState(0)
  const rerender = useCallback(() => bump((n) => n + 1), [])

  // Load the rules WASM name-gate once (best-effort; null ⇒ trust the TS mirror).
  useEffect(() => {
    let live = true
    loadRulesWasm().then((w) => {
      if (live) wasmGate.current = w
    })
    return () => {
      live = false
    }
  }, [])

  const raiseCorrection = useCallback((reason: string) => {
    flash.current = 1
    setCorrection(reason)
  }, [])

  // (Re)establish the online socket, or tear it down for practice mode.
  useEffect(() => {
    if (mode === 'practice') {
      connection.current?.close()
      connection.current = null
      setStatus('practice')
      return
    }

    const conn = new MatchConnection({
      onStatus: setStatus,
      onMessage: (message) => {
        const r = reconciler.current
        switch (message.type) {
          case 'ack':
            if (message.accepted) r.confirmHead()
            else {
              const rolled = r.rejectHead()
              raiseCorrection(rolled.rolledBack ? `Server rejected your move: ${message.reason ?? 'illegal'}` : (message.reason ?? 'move corrected'))
            }
            break
          case 'delta': {
            const c = r.applyAuthoritative(message.events)
            if (c.dropped.length) raiseCorrection('Board reconciled to server state')
            break
          }
          case 'snapshot': {
            const c = r.reset(message.state)
            if (c.dropped.length) raiseCorrection('Board resynced to server state')
            break
          }
        }
        rerender()
      },
    })
    connection.current = conn
    conn.connect()
    return () => conn.close()
  }, [mode, raiseCorrection, rerender])

  // Canvas renderer + animation loop. Redraws from refs each frame and decays the
  // correction flash; a ResizeObserver keeps the backing store matched to the box.
  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    const renderer = new BoardRenderer(canvas)
    rendererRef.current = renderer

    const observer = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(() => renderer.resize()) : null
    observer?.observe(canvas)

    let frame = 0
    const draw = () => {
      if (flash.current > 0) flash.current = Math.max(0, flash.current - 0.04)
      renderer.render(reconciler.current.view(), { selfSeat: SELF_SEAT, correctionFlash: flash.current })
      frame = requestAnimationFrame(draw)
    }
    frame = requestAnimationFrame(draw)

    return () => {
      cancelAnimationFrame(frame)
      observer?.disconnect()
      rendererRef.current = null
    }
  }, [])

  /**
   * Run an action through the two-stage optimistic gate and dispatch it. Rejected
   * moves never leave the client; accepted ones apply locally, then either send
   * (online) or self-confirm (practice).
   */
  const dispatch = useCallback(
    (action: MatchAction) => {
      // Stage 1: authoritative rules-WASM name-gate (when the crate is loaded).
      if (wasmGate.current && !wasmGate.current.recognizes(action.kind as CommandName)) {
        raiseCorrection(`Rules engine does not recognize ${action.kind}`)
        rerender()
        return
      }
      // Stage 2: local validation (Juice, Heat, turn, board caps).
      const result = reconciler.current.predict(action)
      if (!result.ok) {
        raiseCorrection(result.reason)
        rerender()
        return
      }
      if (mode === 'practice') {
        // Offline: the local rules are the authority — confirm immediately.
        reconciler.current.confirmHead()
      } else if (!connection.current?.send(action)) {
        // Socket not open: keep the prediction pending; resync reconciles it.
        setStatus((s) => (s === 'open' ? 'reconnecting' : s))
      }
      rerender()
    },
    [mode, raiseCorrection, rerender],
  )

  const playCard = useCallback(() => {
    cardSeq.current += 1
    dispatch({ kind: 'PlayCardCmd', seat: SELF_SEAT, cardInstanceId: `card-${cardSeq.current}`, targetRef: 'boss:B', juiceCost: 1 })
  }, [dispatch])

  const heroPower = useCallback(() => {
    dispatch({ kind: 'ActivateHeroPowerCmd', seat: SELF_SEAT, targetRef: 'boss:B', juiceCost: 2 })
  }, [dispatch])

  const endTurn = useCallback(() => dispatch({ kind: 'EndTurnCmd', seat: SELF_SEAT }), [dispatch])
  const concede = useCallback(() => dispatch({ kind: 'ConcedeMatchCmd', seat: SELF_SEAT }), [dispatch])

  const newMatch = useCallback(() => {
    reconciler.current.reset(startMatch(matchId))
    cardSeq.current = 0
    setCorrection(null)
    rerender()
  }, [matchId, rerender])

  const setMode = useCallback(
    (next: MatchMode) => {
      setModeState(next)
      reconciler.current.reset(startMatch(matchId))
      cardSeq.current = 0
      setCorrection(null)
      rerender()
    },
    [matchId, rerender],
  )

  const dismissCorrection = useCallback(() => setCorrection(null), [])

  const state = reconciler.current.view()

  return useMemo(
    () => ({
      state,
      mode,
      selfSeat: SELF_SEAT,
      status,
      pending: reconciler.current.pendingCount(),
      correction,
      canvasRef,
      playCard,
      heroPower,
      endTurn,
      concede,
      setMode,
      newMatch,
      dismissCorrection,
    }),
    [state, mode, status, correction, playCard, heroPower, endTurn, concede, setMode, newMatch, dismissCorrection],
  )
}
