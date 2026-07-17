/**
 * MatchConnection transport tests — the client→server command wire.
 *
 * The reconciler predicts locally; the connection is what actually reaches the
 * authoritative server. This asserts `send()` ships the full structured envelope
 * the server's `ClientMessage::Action` requires
 * (`{ type:"action", matchId, playerId, command, payload }`) — including the
 * mandatory `playerId` (no serde default) — rather than the bare command `kind`
 * the scaffold once accepted. That is the fix that closes the client↔server
 * command drift end-to-end and actually activates the online command path.
 */
import { describe, expect, it, vi } from 'vitest'
import { MatchConnection, type ConnectionHandlers } from './connection'
import type { MatchAction } from './model'

// A minimal fake WebSocket in the OPEN state whose `send` records each frame.
class FakeSocket {
  static readonly OPEN = 1
  readyState = FakeSocket.OPEN
  readonly sent: string[] = []
  send(frame: string): void {
    this.sent.push(frame)
  }
  close(): void {}
}

/** Wire a MatchConnection whose live socket is a spyable FakeSocket. */
function makeTestConnection(matchId = 'm-42', playerId = 'm-42-a'): { conn: MatchConnection; socket: FakeSocket } {
  const handlers: ConnectionHandlers = { onMessage: vi.fn(), onStatus: vi.fn() }
  const conn = new MatchConnection(handlers, matchId, playerId)
  const socket = new FakeSocket()
  // Inject the fake as the connection's live socket (bypassing the real open()).
  ;(conn as unknown as { socket: FakeSocket }).socket = socket
  return { conn, socket }
}

describe('MatchConnection.send', () => {
  it('ships the full structured envelope the server requires', () => {
    const { conn, socket } = makeTestConnection('m-42', 'm-42-a')
    const action: MatchAction = { kind: 'AttackCmd', seat: 'A', attackerId: 'A-atk', targetRef: 'boss:B' }

    const ok = conn.send(action)

    expect(ok).toBe(true)
    expect(socket.sent).toHaveLength(1)
    const env = JSON.parse(socket.sent[0])

    // Assert the EXACT required-field set of `ClientMessage::Action`. This must
    // FAIL if any field (notably `playerId`, which is mandatory server-side with
    // no serde default) is ever dropped — a partial `toMatchObject` would not.
    expect(Object.keys(env).sort()).toEqual(['command', 'matchId', 'payload', 'playerId', 'type'])
    expect(env.type).toBe('action')
    expect(env.matchId).toBe('m-42')
    // `playerId` names the local player's Outfit; the server's `seat_for_player`
    // resolves it, and the frame will not deserialize at all without it.
    expect(env.playerId).toBe('m-42-a')
    expect(typeof env.playerId).toBe('string')
    expect(env.playerId.length).toBeGreaterThan(0)
    expect(env.command).toBe('AttackCmd')
    expect(env.payload).toEqual({ seat: 'A', attackerId: 'A-atk', targetRef: 'boss:B' })
    // The command carries no bare `kind` — it moved into `command`.
    expect(env.payload.kind).toBeUndefined()
  })

  it('returns false and sends nothing when the socket is not open', () => {
    const { conn, socket } = makeTestConnection()
    socket.readyState = 0 // CONNECTING

    const ok = conn.send({ kind: 'EndTurnCmd', seat: 'A' })

    expect(ok).toBe(false)
    expect(socket.sent).toHaveLength(0)
  })
})
