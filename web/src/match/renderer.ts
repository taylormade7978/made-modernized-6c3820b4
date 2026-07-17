/**
 * HTML5 Canvas board renderer.
 *
 * Given a {@link MatchState} it paints the whole board: both seats' Boss HP,
 * Heat, and Juice meters, their Operator/Vehicle slots, a turn indicator, and —
 * when a mispredicted move is reconciled away — a red correction flash. It is a
 * pure *view*: it never mutates state and holds no game logic, so the same
 * frame renders identically from an optimistic prediction or an authoritative
 * delta.
 *
 * {@link BoardRenderer} owns the device-pixel-ratio bookkeeping (drawing in CSS
 * pixels while backing the canvas at native resolution) so the board stays crisp
 * and legible on a high-density mobile display. Layout is a vertical split —
 * opponent on top, the local seat on the bottom — which reads naturally when the
 * phone is held upright.
 */
import { HEAT_MAX, MAX_OPERATORS, MAX_VEHICLES, opponent, type MatchState, type Seat, type SeatState } from './model'

/** Per-frame render inputs beyond the board state itself. */
export interface RenderOptions {
  /** The seat the local player controls (drawn on the bottom half). */
  readonly selfSeat: Seat
  /** Correction-flash intensity in `[0, 1]`; 0 = no flash (see rollback). */
  readonly correctionFlash: number
}

const COLORS = {
  bg: '#0b0d12',
  panel: '#151a23',
  panelEdge: 'rgba(255,255,255,0.08)',
  text: '#e7ecf3',
  muted: '#8b97a8',
  accent: '#4c8bf5',
  boss: '#e05b6a',
  heat: '#f0a24c',
  juice: '#4cc9f0',
  slot: 'rgba(255,255,255,0.05)',
  flash: '#e05b6a',
} as const

export class BoardRenderer {
  private readonly ctx: CanvasRenderingContext2D
  private width = 0
  private height = 0

  constructor(private readonly canvas: HTMLCanvasElement) {
    const ctx = canvas.getContext('2d')
    if (!ctx) throw new Error('2D canvas context unavailable')
    this.ctx = ctx
    this.resize()
  }

  /**
   * Match the backing store to the element's CSS box × device pixel ratio, so a
   * retina phone draws at native resolution. Call on mount and on every resize.
   */
  resize(): void {
    const dpr = typeof window === 'undefined' ? 1 : window.devicePixelRatio || 1
    const rect = this.canvas.getBoundingClientRect()
    this.width = rect.width || this.canvas.clientWidth || 360
    this.height = rect.height || this.canvas.clientHeight || 540
    this.canvas.width = Math.round(this.width * dpr)
    this.canvas.height = Math.round(this.height * dpr)
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
  }

  /** Paint one frame of `state` from the local player's point of view. */
  render(state: MatchState, options: RenderOptions): void {
    const { ctx, width: w, height: h } = this
    ctx.clearRect(0, 0, w, h)
    ctx.fillStyle = COLORS.bg
    ctx.fillRect(0, 0, w, h)

    const self = options.selfSeat
    const foe = opponent(self)
    const half = h / 2
    const pad = 12

    // Opponent occupies the top half (drawn "facing" the player); local seat the
    // bottom half.
    this.drawSeat(state, foe, pad, pad, w - pad * 2, half - pad * 1.5, false)
    this.drawSeat(state, self, pad, half + pad * 0.5, w - pad * 2, half - pad * 1.5, true)

    this.drawTurnBanner(state, options, w, half)

    if (state.phase === 'completed') this.drawOutcome(state, options, w, h)
    if (options.correctionFlash > 0) this.drawFlash(options.correctionFlash, w, h)
  }

  /** Draw one seat's panel: identity + meters + board slots. */
  private drawSeat(
    state: MatchState,
    seat: Seat,
    x: number,
    y: number,
    w: number,
    h: number,
    isSelf: boolean,
  ): void {
    const { ctx } = this
    const s = state.seats[seat]
    const active = state.turn === seat

    roundRect(ctx, x, y, w, h, 12)
    ctx.fillStyle = COLORS.panel
    ctx.fill()
    ctx.lineWidth = active ? 2 : 1
    ctx.strokeStyle = active ? COLORS.accent : COLORS.panelEdge
    ctx.stroke()

    const innerX = x + 14
    let cursorY = y + 22

    // Boss name + a "you"/"opponent" tag.
    ctx.textBaseline = 'alphabetic'
    ctx.fillStyle = COLORS.text
    ctx.font = '600 15px system-ui, sans-serif'
    ctx.fillText(s.bossName, innerX, cursorY)
    ctx.font = '11px system-ui, sans-serif'
    ctx.fillStyle = COLORS.muted
    ctx.fillText(isSelf ? 'You' : 'Opponent', innerX, cursorY + 15)

    cursorY += 30
    // Meters: Boss HP, Heat, Juice.
    const meterW = w - 28
    this.drawMeter(innerX, cursorY, meterW, `Boss HP ${s.bossHp}`, s.bossHp, 30, COLORS.boss)
    this.drawMeter(innerX, cursorY + 26, meterW, `Heat ${s.heat}/${HEAT_MAX}`, s.heat, HEAT_MAX, COLORS.heat)
    // The denominator is the seat's Juice crystal (what the pool refills to),
    // not the static hard cap — so the grown crystal is visible to the player.
    this.drawMeter(innerX, cursorY + 52, meterW, `Juice ${s.juice}/${s.maxJuice}`, s.juice, s.maxJuice, COLORS.juice)

    // Board: operator + vehicle slots.
    this.drawBoard(s, innerX, cursorY + 74, meterW, y + h - (cursorY + 74) - 10)
  }

  /** A labelled horizontal bar filled to `value / max`. */
  private drawMeter(x: number, y: number, w: number, label: string, value: number, max: number, color: string): void {
    const { ctx } = this
    const barY = y + 4
    const barH = 8
    roundRect(ctx, x, barY, w, barH, 4)
    ctx.fillStyle = COLORS.slot
    ctx.fill()
    const frac = max <= 0 ? 0 : Math.max(0, Math.min(1, value / max))
    if (frac > 0) {
      roundRect(ctx, x, barY, Math.max(barH, w * frac), barH, 4)
      ctx.fillStyle = color
      ctx.fill()
    }
    ctx.fillStyle = COLORS.muted
    ctx.font = '10px system-ui, sans-serif'
    ctx.fillText(label, x, y - 2)
  }

  /** Draw the Operator (up to 7) and Vehicle (up to 3) board slots. */
  private drawBoard(s: SeatState, x: number, y: number, w: number, h: number): void {
    if (h < 24) return
    const rowH = Math.min(28, (h - 6) / 2)
    this.drawSlotRow(x, y, w, rowH, s.operators, MAX_OPERATORS, COLORS.accent)
    this.drawSlotRow(x, y + rowH + 6, w, rowH, s.vehicles, MAX_VEHICLES, COLORS.juice)
  }

  /** A row of `max` slots, `filled` of them occupied (a card on the board). */
  private drawSlotRow(x: number, y: number, w: number, h: number, filled: number, max: number, color: string): void {
    const { ctx } = this
    const gap = 4
    const slotW = (w - gap * (max - 1)) / max
    for (let i = 0; i < max; i += 1) {
      const sx = x + i * (slotW + gap)
      roundRect(ctx, sx, y, slotW, h, 5)
      if (i < filled) {
        ctx.fillStyle = color
        ctx.globalAlpha = 0.85
        ctx.fill()
        ctx.globalAlpha = 1
      } else {
        ctx.fillStyle = COLORS.slot
        ctx.fill()
      }
    }
  }

  /** Center turn indicator between the two seat panels. */
  private drawTurnBanner(state: MatchState, options: RenderOptions, w: number, midY: number): void {
    const { ctx } = this
    let text: string
    if (state.phase === 'completed') text = 'Match over'
    else if (state.turn === null) text = 'Waiting…'
    else text = state.turn === options.selfSeat ? 'Your turn' : "Opponent's turn"

    ctx.font = '600 12px system-ui, sans-serif'
    const tw = ctx.measureText(text).width
    const bx = (w - tw) / 2 - 10
    roundRect(ctx, bx, midY - 11, tw + 20, 22, 11)
    ctx.fillStyle = COLORS.panel
    ctx.fill()
    ctx.strokeStyle = COLORS.panelEdge
    ctx.lineWidth = 1
    ctx.stroke()
    ctx.fillStyle = state.turn === options.selfSeat ? COLORS.accent : COLORS.muted
    ctx.fillText(text, bx + 10, midY + 4)
  }

  /** Win/loss overlay once the match completes. */
  private drawOutcome(state: MatchState, options: RenderOptions, w: number, h: number): void {
    const { ctx } = this
    ctx.fillStyle = 'rgba(11,13,18,0.72)'
    ctx.fillRect(0, 0, w, h)
    const won = state.winner === options.selfSeat
    ctx.textAlign = 'center'
    ctx.fillStyle = won ? COLORS.accent : COLORS.boss
    ctx.font = '700 26px system-ui, sans-serif'
    ctx.fillText(won ? 'Victory' : 'Defeat', w / 2, h / 2)
    ctx.textAlign = 'start'
  }

  /** Red edge flash pulsed when an optimistic move was rolled back. */
  private drawFlash(intensity: number, w: number, h: number): void {
    const { ctx } = this
    ctx.save()
    ctx.globalAlpha = Math.min(0.6, intensity * 0.6)
    ctx.lineWidth = 6
    ctx.strokeStyle = COLORS.flash
    ctx.strokeRect(3, 3, w - 6, h - 6)
    ctx.restore()
  }
}

/** Path a rounded rectangle (kept for older canvas engines without roundRect). */
function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number): void {
  const radius = Math.min(r, w / 2, h / 2)
  ctx.beginPath()
  ctx.moveTo(x + radius, y)
  ctx.arcTo(x + w, y, x + w, y + h, radius)
  ctx.arcTo(x + w, y + h, x, y + h, radius)
  ctx.arcTo(x, y + h, x, y, radius)
  ctx.arcTo(x, y, x + w, y, radius)
  ctx.closePath()
}
