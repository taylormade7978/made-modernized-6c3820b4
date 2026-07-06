/**
 * WebSocket transport to the authoritative game server.
 *
 * This is a thin driving adapter: it opens the socket at the handshake URL the
 * API client builds (carrying the edge session cookie on the upgrade — no token
 * in the URL), forwards player actions, and normalizes inbound frames into the
 * typed {@link ServerMessage} the reconciler understands. All game logic lives
 * above it (in {@link Reconciler}); this file only moves bytes and reconnects.
 *
 * It is deliberately tolerant of the scaffold server, which replies to each
 * command with a bare `ok` / error-string *ack* rather than a JSON delta. A
 * frame that parses as JSON is taken as a structured {@link ServerMessage};
 * anything else is read as an ack (`ok` ⇒ accepted, any other text ⇒ rejected
 * with that text as the reason), so optimistic reconciliation works against the
 * server as it exists today and against a fuller delta-pushing server later.
 */
import { api } from '../api'
import type { MatchAction, MatchState, ServerMessage } from './model'

/** Observable transport status, surfaced to the UI. */
export type ConnectionStatus = 'connecting' | 'open' | 'reconnecting' | 'closed'

export interface ConnectionHandlers {
  readonly onMessage: (message: ServerMessage) => void
  readonly onStatus: (status: ConnectionStatus) => void
}

/** Reconnect backoff bounds (ms). Full-jitter exponential, capped. */
const BASE_RECONNECT_MS = 500
const MAX_RECONNECT_MS = 10_000

export class MatchConnection {
  private socket: WebSocket | null = null
  private closedByCaller = false
  private attempt = 0
  private reconnectTimer: ReturnType<typeof setTimeout> | undefined

  constructor(
    private readonly handlers: ConnectionHandlers,
    private readonly ticket?: string,
  ) {}

  /** Open the socket (idempotent while one is already live). */
  connect(): void {
    if (this.socket && this.socket.readyState <= WebSocket.OPEN) return
    this.closedByCaller = false
    this.open()
  }

  /** Close for good — no reconnect follows a caller-initiated close. */
  close(): void {
    this.closedByCaller = true
    if (this.reconnectTimer !== undefined) clearTimeout(this.reconnectTimer)
    this.socket?.close()
    this.socket = null
    this.handlers.onStatus('closed')
  }

  /**
   * Forward a player action to the authoritative server. The current server
   * parses each text frame as a command name, so we send the action's wire
   * `kind` verbatim; a richer server can decode the same name plus a payload.
   * Returns `false` when the socket is not open (the caller keeps the prediction
   * pending and the reconnect/resync path reconciles it).
   */
  send(action: MatchAction): boolean {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return false
    this.socket.send(action.kind)
    return true
  }

  private open(): void {
    const url = api.realtime.gameSocketUrl(this.ticket ? { ticket: this.ticket } : undefined)
    this.handlers.onStatus(this.attempt === 0 ? 'connecting' : 'reconnecting')

    const socket = new WebSocket(url)
    this.socket = socket

    socket.onopen = () => {
      this.attempt = 0
      this.handlers.onStatus('open')
    }
    socket.onmessage = (event) => {
      const message = parseFrame(event.data)
      if (message) this.handlers.onMessage(message)
    }
    socket.onclose = () => {
      this.socket = null
      if (!this.closedByCaller) this.scheduleReconnect()
    }
    // A socket error is followed by `onclose`; let the close path reconnect.
    socket.onerror = () => socket.close()
  }

  private scheduleReconnect(): void {
    this.handlers.onStatus('reconnecting')
    const delay = backoff(this.attempt)
    this.attempt += 1
    this.reconnectTimer = setTimeout(() => {
      if (!this.closedByCaller) this.open()
    }, delay)
  }
}

/** Full-jitter exponential backoff, capped at {@link MAX_RECONNECT_MS}. */
function backoff(attempt: number): number {
  const exp = Math.min(MAX_RECONNECT_MS, BASE_RECONNECT_MS * 2 ** attempt)
  return Math.round(Math.random() * exp)
}

/**
 * Normalize a raw WebSocket frame into a {@link ServerMessage}. Structured JSON
 * frames are validated shallowly against the three envelope shapes; a non-JSON
 * frame is read as an ack (`ok` ⇒ accepted, otherwise a rejection carrying the
 * server's error text). Returns `null` for a frame we cannot interpret.
 */
export function parseFrame(data: unknown): ServerMessage | null {
  if (typeof data !== 'string') return null
  const text = data.trim()
  if (!text) return null

  if (text.startsWith('{')) {
    try {
      return asServerMessage(JSON.parse(text))
    } catch {
      return null
    }
  }

  // Bare ack from the scaffold server.
  if (text === 'ok') return { type: 'ack', accepted: true }
  return { type: 'ack', accepted: false, reason: text }
}

/** Validate a parsed JSON value against the {@link ServerMessage} envelopes. */
function asServerMessage(value: unknown): ServerMessage | null {
  if (typeof value !== 'object' || value === null) return null
  const msg = value as Record<string, unknown>
  switch (msg.type) {
    case 'snapshot':
      return msg.state ? { type: 'snapshot', state: msg.state as MatchState } : null
    case 'delta':
      return Array.isArray(msg.events) ? { type: 'delta', events: msg.events } : null
    case 'ack':
      return { type: 'ack', accepted: msg.accepted === true, reason: typeof msg.reason === 'string' ? msg.reason : undefined }
    default:
      return null
  }
}
