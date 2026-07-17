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
import { aiTurn, startMatch } from './rules'
import { loadRulesWasm, type RulesWasm } from './wasm'
import { opponent, type CommandName, type MatchAction, type MatchState, type Seat } from './model'

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
  readonly playCard: (cardInstanceId: string, targetRef?: string) => void
  readonly attack: (attackerId: string, targetRef: string) => void
  readonly heroPower: () => void
  readonly endTurn: () => void
  readonly concede: () => void
  readonly setMode: (mode: MatchMode) => void
  readonly newMatch: () => void
  readonly dismissCorrection: () => void
}

export function useMatch(matchId = 'live', ticket?: string): MatchController {
  const reconciler = useRef<Reconciler>(new Reconciler(startMatch(matchId)))
  const connection = useRef<MatchConnection | null>(null)
  const wasmGate = useRef<RulesWasm | null>(null)
  const cardSeq = useRef(0)
  const flash = useRef(0)

  const canvasRef = useRef<HTMLCanvasElement>(null)
  const rendererRef = useRef<BoardRenderer | null>(null)

  // Default to Practice: the local rules WASM is a self-contained authority, so
  // the board is immediately playable with no server. Online attaches the
  // authoritative socket when a backend is available.
  const [mode, setModeState] = useState<MatchMode>('practice')
  const [status, setStatus] = useState<ConnectionStatus | 'practice'>('practice')
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

    // The local player's identity is the name of the Outfit it is seated as —
    // `<matchId>-a` for seat A — matching the domain's `OutfitConfig` naming
    // (`startMatch` above) and what the server's `seat_for_player` resolves.
    const selfPlayerId = `${matchId}-${SELF_SEAT.toLowerCase()}`

    const conn = new MatchConnection(
      {
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
      },
      // The match this connection acts on; rides in every action envelope.
      matchId,
      // The local player's identity; the server requires it on every action.
      selfPlayerId,
      // A mission launch joins the AI-opponent's authoritative match via its ticket.
      ticket,
    )
    connection.current = conn
    conn.connect()
    return () => conn.close()
  }, [mode, matchId, ticket, raiseCorrection, rerender])

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
      // Stage 1: authoritative rules-WASM name-gate. Only meaningful ONLINE,
      // where it proves the browser and server share one rules binary. In
      // practice the local TS mirror is the sole authority (and stays current
      // with the client's command set, e.g. AttackCmd), so the gate — which may
      // be a stale compiled crate — must not veto a command the mirror handles.
      if (mode === 'online' && wasmGate.current && !wasmGate.current.recognizes(action.kind as CommandName)) {
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

  // In practice, after the local player hands over the turn, run the opponent's
  // whole turn through the same rules (aiTurn loops until it passes back or wins)
  // and fold its authoritative-shaped deltas into the board.
  const runAiTurn = useCallback(() => {
    if (mode !== 'practice') return
    let guard = 0
    while (guard++ < 100) {
      const view = reconciler.current.view()
      if (view.phase !== 'active' || view.turn !== opponent(SELF_SEAT)) break
      const { events } = aiTurn(view, opponent(SELF_SEAT))
      if (!events.length) break
      reconciler.current.applyAuthoritative(events)
    }
    rerender()
  }, [mode, rerender])

  const playCard = useCallback(
    (cardInstanceId: string, targetRef = 'boss:B') => {
      const card = reconciler.current.view().seats[SELF_SEAT].hand.find((c) => c.instanceId === cardInstanceId)
      if (!card) return
      dispatch({ kind: 'PlayCardCmd', seat: SELF_SEAT, cardInstanceId, targetRef, juiceCost: card.cost })
    },
    [dispatch],
  )

  const attack = useCallback(
    (attackerId: string, targetRef: string) => dispatch({ kind: 'AttackCmd', seat: SELF_SEAT, attackerId, targetRef }),
    [dispatch],
  )

  const heroPower = useCallback(() => {
    dispatch({ kind: 'ActivateHeroPowerCmd', seat: SELF_SEAT, targetRef: 'boss:B', juiceCost: 2 })
  }, [dispatch])

  const endTurn = useCallback(() => {
    dispatch({ kind: 'EndTurnCmd', seat: SELF_SEAT })
    // Let the board paint the hand-off, then run the opponent's turn.
    setTimeout(runAiTurn, 500)
  }, [dispatch, runAiTurn])
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
      attack,
      heroPower,
      endTurn,
      concede,
      setMode,
      newMatch,
      dismissCorrection,
    }),
    [state, mode, status, correction, playCard, attack, heroPower, endTurn, concede, setMode, newMatch, dismissCorrection],
  )
}
