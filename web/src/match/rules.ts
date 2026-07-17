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
  COP_EVENT_DAMAGE,
  COP_EVENT_RESET_TO,
  COP_EVENT_THRESHOLD,
  defaultOutfit,
  HEAT_MAX,
  HEAT_MIN,
  hasSpotlight,
  HEAT_PER_PLAY,
  JUICE_CAP,
  JUICE_RAMP_PER_TURN,
  opponent,
  seatFromOutfit,
  type BoardUnit,
  type CardDef,
  type DeltaEvent,
  type HandCard,
  type MatchAction,
  type MatchState,
  type OutfitConfig,
  type Seat,
  type SeatState,
} from './model'

// ── Card pool & deck building ──────────────────────────────────────────────────
// A curated slice of the real catalog with resolvable effects (the client rules
// only model a small, closed effect set — see EffectKind). Practice builds a
// 30-card deck per seat from this list; art/flavor live in the card service.
export const CARD_POOL: readonly CardDef[] = [
  { cardId: 'bolt', name: 'Bolt', cost: 1, type: 'Job', effect: 'damage', amount: 3, text: 'Deal 3 damage to any target.' },
  { cardId: 'w_corner_boy', name: 'Corner Boy', cost: 1, type: 'Operator', effect: 'summon', amount: 0, atk: 1, hp: 2, keywords: ['Stealth'], text: '1/2. Stealth. Cheap eyes on the block.' },
  { cardId: 'pd_beat_cop', name: 'Beat Cop', cost: 1, type: 'Operator', effect: 'summon', amount: 0, atk: 1, hp: 2, text: '1/2. Walks the block.' },
  { cardId: 'w_young_buck', name: 'Young Buck', cost: 1, type: 'Operator', effect: 'summon', amount: 0, atk: 2, hp: 1, text: '2/1. Reckless.' },
  { cardId: 'w_drive_by', name: 'Drive-By', cost: 2, type: 'Job', effect: 'damage', amount: 4, text: 'Deal 4 damage to any target.' },
  { cardId: 'w_the_homie', name: 'The Homie', cost: 2, type: 'Operator', effect: 'summon', amount: 0, atk: 3, hp: 2, text: '3/2. Loyal muscle.' },
  { cardId: 'w_the_enforcer', name: 'The Enforcer', cost: 3, type: 'Operator', effect: 'summon', amount: 0, atk: 2, hp: 5, keywords: ['Spotlight'], text: '2/5. Spotlight — enemies must deal with it first.' },
  { cardId: 'pd_riot_squad', name: 'Riot Squad', cost: 5, type: 'Operator', effect: 'summon', amount: 0, atk: 4, hp: 5, keywords: ['Spotlight'], text: '4/5. Spotlight.' },
  { cardId: 'pd_the_crib', name: 'The Crib', cost: 2, type: 'Piece', effect: 'cool', amount: 2, text: 'Lower your Heat by 2.' },
  { cardId: 'ht_the_come_up', name: 'The Come-Up', cost: 2, type: 'Piece', effect: 'juice', amount: 2, text: 'Gain 2 Juice this turn.' },
  { cardId: 'w_stolen_whip', name: 'Stolen Whip', cost: 3, type: 'Vehicle', effect: 'summon', amount: 2, atk: 4, hp: 3, keywords: ['Drive-By'], text: '4/3. Drive-By: deal 2 to the enemy boss on arrival.' },
  { cardId: 'w_blow_the_safe', name: 'Blow the Safe', cost: 3, type: 'Job', effect: 'draw', amount: 2, text: 'Draw 2 cards.' },
  { cardId: 'w_shot_caller', name: 'Shot Caller', cost: 4, type: 'Operator', effect: 'summon', amount: 0, atk: 5, hp: 5, text: '5/5. Runs the crew.' },
  { cardId: 'w_the_big_one', name: 'The Big One', cost: 5, type: 'Heist', effect: 'damage', amount: 7, text: 'Deal 7 damage to any target.' },
]

/** Opening hand size dealt to each seat. */
export const OPENING_HAND = 4

/** A tiny deterministic PRNG (mulberry32) so a seed reproduces the same shuffle. */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0
  return () => {
    a = (a + 0x6d2b79f5) | 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

/** Build a shuffled 30-card deck of instanced cards for `seat`, seeded. */
export function buildDeck(seed: number, seat: Seat): HandCard[] {
  const rng = mulberry32(seed ^ (seat === 'A' ? 0x1111 : 0x2222))
  const cards: HandCard[] = []
  let n = 0
  while (cards.length < 30) {
    const def = CARD_POOL[Math.floor(rng() * CARD_POOL.length)]
    cards.push({ ...def, instanceId: `${seat}-${def.cardId}-${n++}` })
  }
  // Fisher–Yates with the same seeded stream.
  for (let i = cards.length - 1; i > 0; i--) {
    const j = Math.floor(rng() * (i + 1))
    ;[cards[i], cards[j]] = [cards[j], cards[i]]
  }
  return cards
}

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
  const deal = (outfit: OutfitConfig, seat: Seat): SeatState => {
    const deck = buildDeck(rngSeed, seat)
    const hand = deck.slice(0, OPENING_HAND)
    return { ...seatFromOutfit(outfit), hand, deck: deck.slice(OPENING_HAND), board: [], operators: 0, vehicles: 0 }
  }
  return {
    matchId,
    seats: { A: deal(playerA, 'A'), B: deal(playerB, 'B') },
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
      const card = seat.hand.find((c) => c.instanceId === action.cardInstanceId)
      if (!card) return reject('that card is not in your hand')
      if (card.cost > seat.juice) {
        return reject(`not enough Juice (need ${card.cost}, have ${seat.juice})`)
      }
      // Playing a card always raises Heat; the raise may not leave the bounds.
      if (seat.heat + HEAT_PER_PLAY > HEAT_MAX) {
        return reject('too much Heat — a Cop Event must resolve first')
      }
      return OK
    }
    case 'AttackCmd': {
      const unit = seat.board.find((u) => u.instanceId === action.attackerId)
      if (!unit) return reject('no such Operator on your board')
      if (!unit.ready) return reject('that Operator can’t attack yet')
      const foe = state.seats[opponent(action.seat)]
      if (action.targetRef === `boss:${action.seat}`) return reject('you can’t attack your own boss')
      const targetOp = action.targetRef.startsWith('op:') ? foe.board.find((u) => u.instanceId === action.targetRef.slice(3)) : null
      if (action.targetRef.startsWith('op:') && !targetOp) return reject('no such target')
      // Spotlight (taunt): while the enemy has one, it must be the target.
      const spotlights = foe.board.filter(hasSpotlight)
      if (spotlights.length && !(targetOp && hasSpotlight(targetOp))) {
        return reject('must attack a Spotlight Operator first')
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

  // Build the delta list by folding as we go, so each step sees the effect of
  // the previous one (needed for the Cop-Event and win checks below).
  const events: DeltaEvent[] = []
  let cur = state
  const emit = (e: DeltaEvent) => {
    events.push(e)
    cur = foldEvent(cur, e)
  }
  const me = action.seat
  const foe = opponent(me)

  switch (action.kind) {
    case 'PlayCardCmd': {
      const card = cur.seats[me].hand.find((c) => c.instanceId === action.cardInstanceId)!
      emit({ type: 'card.played', player: me, cardInstanceId: card.instanceId, targetRef: `boss:${foe}`, juiceSpent: card.cost })
      emit({ type: 'heat.raised', player: me, amount: HEAT_PER_PLAY, newHeat: clamp(cur.seats[me].heat + HEAT_PER_PLAY, HEAT_MIN, HEAT_MAX) })
      resolveEffect(cur, emit, me, card, action.targetRef)
      // A Cop Event fires when the play tips you to the threshold: it raids the
      // hottest player (that's you, this play) and cools Heat back down.
      if (cur.seats[me].heat >= COP_EVENT_THRESHOLD) {
        emit({ type: 'cop.raided', player: me, bossHp: Math.max(0, cur.seats[me].bossHp - COP_EVENT_DAMAGE), newHeat: COP_EVENT_RESET_TO })
      }
      checkWin(cur, emit)
      break
    }
    case 'AttackCmd': {
      const attacker = cur.seats[me].board.find((u) => u.instanceId === action.attackerId)!
      // Capture both attack values BEFORE damage so combat is simultaneous.
      const defender = action.targetRef.startsWith('op:') ? findUnit(cur, action.targetRef.slice(3)) : null
      const atkDmg = attacker.atk
      const retaliation = defender ? defender.unit.atk : 0
      for (const e of damageTargetEvents(cur, action.targetRef, atkDmg)) emit(e)
      // A defending Operator strikes back at the attacker.
      if (defender && retaliation > 0) {
        for (const e of damageTargetEvents(cur, `op:${action.attackerId}`, retaliation)) emit(e)
      }
      // The attacker exhausts (if it survived the trade).
      if (cur.seats[me].board.some((u) => u.instanceId === action.attackerId)) {
        emit({ type: 'operator.exhausted', player: me, instanceId: action.attackerId })
      }
      checkWin(cur, emit)
      break
    }
    case 'ActivateHeroPowerCmd': {
      emit({ type: 'hero_power.activated', player: me, targetRef: action.targetRef, juiceSpent: action.juiceCost, remainingJuice: clamp(cur.seats[me].juice - action.juiceCost, 0, JUICE_CAP) })
      // Boss Power: a reliable 2-damage poke at the enemy boss.
      emit({ type: 'boss.damaged', player: foe, amount: 2, newHp: Math.max(0, cur.seats[foe].bossHp - 2) })
      checkWin(cur, emit)
      break
    }
    case 'EndTurnCmd': {
      const next = foe
      emit({ type: 'operators.readied', player: next })
      const top = cur.seats[next].deck[0]
      if (top) emit({ type: 'card.drawn', player: next, card: top })
      // The available pool ramps (+1, capped) and so does the crystal it refills
      // to — mirroring the aggregate's `next_player_juice`/`next_player_max_juice`.
      emit({
        type: 'turn.ended',
        player: me,
        nextPlayer: next,
        nextPlayerJuice: clamp(cur.seats[next].juice + JUICE_RAMP_PER_TURN, 0, JUICE_CAP),
        nextPlayerMaxJuice: clamp(cur.seats[next].maxJuice + JUICE_RAMP_PER_TURN, 0, JUICE_CAP),
      })
      break
    }
    case 'ConcedeMatchCmd': {
      emit({ type: 'match.completed', concedingPlayer: me, winner: foe })
      break
    }
  }
  return { state: cur, events }
}

/** Locate a board unit by instance id across both seats. */
export function findUnit(state: MatchState, id: string): { seat: Seat; unit: BoardUnit } | null {
  for (const seat of ['A', 'B'] as const) {
    const unit = state.seats[seat].board.find((u) => u.instanceId === id)
    if (unit) return { seat, unit }
  }
  return null
}

/** The damage events (boss or operator, incl. death) for hitting `targetRef`. */
function damageTargetEvents(state: MatchState, targetRef: string, amount: number): DeltaEvent[] {
  if (amount <= 0) return []
  if (targetRef.startsWith('boss:')) {
    const seat = targetRef.slice(5) as Seat
    if (seat !== 'A' && seat !== 'B') return []
    return [{ type: 'boss.damaged', player: seat, amount, newHp: Math.max(0, state.seats[seat].bossHp - amount) }]
  }
  if (targetRef.startsWith('op:')) {
    const found = findUnit(state, targetRef.slice(3))
    if (!found) return []
    const newHp = found.unit.hp - amount
    const evs: DeltaEvent[] = [{ type: 'operator.damaged', player: found.seat, instanceId: found.unit.instanceId, newHp: Math.max(0, newHp) }]
    if (newHp <= 0) evs.push({ type: 'operator.died', player: found.seat, instanceId: found.unit.instanceId })
    return evs
  }
  return []
}

/** Emit the effect events for a played `card`, aimed at `targetRef`. */
function resolveEffect(state: MatchState, emit: (e: DeltaEvent) => void, me: Seat, card: HandCard, targetRef: string): void {
  const foe = opponent(me)
  switch (card.effect) {
    case 'damage':
      for (const e of damageTargetEvents(state, targetRef || `boss:${foe}`, card.amount)) emit(e)
      break
    case 'summon': {
      emit({
        type: 'operator.summoned',
        player: me,
        unit: { instanceId: card.instanceId, name: card.name, cardId: card.cardId, atk: card.atk ?? 1, hp: card.hp ?? 1, maxHp: card.hp ?? 1, ready: false, keywords: card.keywords ?? [] },
      })
      // Drive-By: the Operator strafes the enemy boss for `amount` as it arrives.
      if ((card.keywords ?? []).includes('Drive-By')) {
        for (const e of damageTargetEvents(state, `boss:${foe}`, card.amount)) emit(e)
      }
      break
    }
    case 'juice':
      emit({ type: 'juice.gained', player: me, amount: card.amount, newJuice: clamp(state.seats[me].juice + card.amount, 0, JUICE_CAP) })
      break
    case 'cool':
      emit({ type: 'heat.set', player: me, newHeat: clamp(state.seats[me].heat - card.amount, HEAT_MIN, HEAT_MAX) })
      break
    case 'draw': {
      const deck = state.seats[me].deck
      for (let i = 0; i < card.amount && i < deck.length; i++) emit({ type: 'card.drawn', player: me, card: deck[i] })
      break
    }
  }
}

/** If either boss has been reduced to 0, complete the match for the other seat. */
function checkWin(state: MatchState, emit: (e: DeltaEvent) => void): void {
  if (state.phase !== 'active') return
  if (state.seats.A.bossHp <= 0) emit({ type: 'match.completed', concedingPlayer: 'A', winner: 'B' })
  else if (state.seats.B.bossHp <= 0) emit({ type: 'match.completed', concedingPlayer: 'B', winner: 'A' })
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
        hand: s.hand.filter((c) => c.instanceId !== event.cardInstanceId),
      }))
    case 'heat.raised':
    case 'heat.set':
      return patchSeat(state, event.player, (s) => ({ ...s, heat: event.newHeat }))
    case 'boss.damaged':
      return patchSeat(state, event.player, (s) => ({ ...s, bossHp: event.newHp }))
    case 'boss.armor.gained':
      // GainArmor raises the activating seat's own Boss HP to the authoritative
      // post-gain value — the mirror of boss.damaged, so an online client folds
      // it instead of desyncing.
      return patchSeat(state, event.player, (s) => ({ ...s, bossHp: event.newHp }))
    case 'juice.gained':
      return patchSeat(state, event.player, (s) => ({ ...s, juice: event.newJuice }))
    case 'operator.summoned':
      return patchSeat(state, event.player, (s) => ({ ...s, board: [...s.board, event.unit], operators: s.board.length + 1 }))
    case 'operator.damaged':
      return patchSeat(state, event.player, (s) => ({ ...s, board: s.board.map((u) => (u.instanceId === event.instanceId ? { ...u, hp: event.newHp } : u)) }))
    case 'operator.died':
      return patchSeat(state, event.player, (s) => {
        const board = s.board.filter((u) => u.instanceId !== event.instanceId)
        return { ...s, board, operators: board.length }
      })
    case 'operators.readied':
      return patchSeat(state, event.player, (s) => ({ ...s, board: s.board.map((u) => ({ ...u, ready: true })) }))
    case 'operator.exhausted':
      return patchSeat(state, event.player, (s) => ({ ...s, board: s.board.map((u) => (u.instanceId === event.instanceId ? { ...u, ready: false } : u)) }))
    case 'cop.raided':
      return patchSeat(state, event.player, (s) => ({ ...s, bossHp: event.bossHp, heat: event.newHeat }))
    case 'card.drawn':
      return patchSeat(state, event.player, (s) => ({
        ...s,
        deck: s.deck.filter((c) => c.instanceId !== event.card.instanceId),
        hand: [...s.hand, event.card],
        deckSize: Math.max(0, s.deckSize - 1),
      }))
    case 'hero_power.activated':
      return patchSeat(state, event.player, (s) => ({ ...s, juice: event.remainingJuice }))
    case 'turn.ended':
      return {
        ...patchSeat(state, event.nextPlayer, (s) => ({ ...s, juice: event.nextPlayerJuice, maxJuice: event.nextPlayerMaxJuice })),
        turn: event.nextPlayer,
      }
    case 'match.completed':
      return { ...state, phase: 'completed', winner: event.winner, turn: null }
  }
}

/**
 * Run a whole AI turn for `seat` in practice: greedily play the most expensive
 * affordable card, swing every ready Operator at the enemy boss, then end the
 * turn. Returns the accumulated state and deltas (folded through the same rules,
 * so the AI can never make an illegal move).
 */
export function aiTurn(state: MatchState, seat: Seat): { state: MatchState; events: DeltaEvent[] } {
  let cur = state
  const events: DeltaEvent[] = []
  const run = (action: MatchAction) => {
    if (!validateAction(cur, action).ok) return false
    const r = applyAction(cur, action)
    cur = r.state
    events.push(...r.events)
    return true
  }
  // Play affordable cards, most expensive first, while it stays our active turn.
  let played = true
  while (played && cur.phase === 'active' && cur.turn === seat) {
    played = false
    const affordable = [...cur.seats[seat].hand]
      .filter((c) => c.cost <= cur.seats[seat].juice && cur.seats[seat].heat + HEAT_PER_PLAY <= HEAT_MAX)
      .sort((a, b) => b.cost - a.cost)
    if (affordable[0]) {
      played = run({ kind: 'PlayCardCmd', seat, cardInstanceId: affordable[0].instanceId, targetRef: `boss:${opponent(seat)}`, juiceCost: affordable[0].cost })
    }
  }
  // Swing every ready Operator: clear a Spotlight blocker first, else go face.
  if (cur.phase === 'active' && cur.turn === seat) {
    const foe = opponent(seat)
    for (const id of cur.seats[seat].board.filter((u) => u.ready).map((u) => u.instanceId)) {
      if (cur.phase !== 'active' || cur.turn !== seat) break
      if (!cur.seats[seat].board.some((u) => u.instanceId === id)) continue // died mid-turn
      const spot = cur.seats[foe].board.filter(hasSpotlight)
      const targetRef = spot.length ? `op:${spot[0].instanceId}` : `boss:${foe}`
      run({ kind: 'AttackCmd', seat, attackerId: id, targetRef })
    }
  }
  if (cur.phase === 'active' && cur.turn === seat) run({ kind: 'EndTurnCmd', seat })
  return { state: cur, events }
}

/** Return a copy of `state` with `seat`'s `SeatState` transformed by `patch`. */
function patchSeat(state: MatchState, seat: Seat, patch: (s: SeatState) => SeatState): MatchState {
  return { ...state, seats: { ...state.seats, [seat]: patch(state.seats[seat]) } }
}
