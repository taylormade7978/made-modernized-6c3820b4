import { useState, type DragEvent } from 'react'
import { useLocation } from 'react-router-dom'
import { useMatch, type MatchMode } from '../match/useMatch'
import { hasSpotlight, opponent, type BoardUnit, type HandCard } from '../match/model'
import type { MissionLaunch } from '../match/mission'

/**
 * Match / board view — the primary play surface.
 *
 * The board is DOM (interactive) over the canvas resource bars: your and the
 * opponent's Operator rows, your hand, and a card-detail panel. Combat is
 * select-then-target — click a ready Operator (or a card that needs a target),
 * then click an enemy Operator or the enemy boss. Spotlight (taunt) and Juice
 * legality are enforced by {@link useMatch}'s rules; illegal picks surface as a
 * correction. A **Practice** toggle runs the whole match client-side vs an AI.
 */
type Sel = { readonly kind: 'attacker'; readonly id: string } | { readonly kind: 'card'; readonly id: string } | null
type Inspected = { readonly card?: HandCard; readonly unit?: BoardUnit } | null

export default function MatchView() {
  const location = useLocation()
  const mission = (location.state as { mission?: MissionLaunch } | null)?.mission
  const match = useMatch('live', mission?.ticket)
  const { state } = match
  const me = match.selfSeat
  const foe = opponent(me)
  const yourTurn = state.turn === me && state.phase === 'active'
  const over = state.phase === 'completed'
  const you = state.seats[me]
  const opp = state.seats[foe]

  const [sel, setSel] = useState<Sel>(null)
  const [inspect, setInspect] = useState<Inspected>(null)
  const [dragId, setDragId] = useState<string | null>(null)
  const targeting = sel !== null
  const enemyHasSpotlight = opp.board.some(hasSpotlight)

  const dragCard = dragId ? you.hand.find((c) => c.instanceId === dragId) ?? null : null
  const dragPlayable = dragCard != null && yourTurn && dragCard.cost <= you.juice && !over
  const allowDrop = (ok: boolean) => (e: DragEvent) => { if (ok) e.preventDefault() }
  // Drop a summon/utility card onto your board; a damage card onto a target.
  const dropToBoard = () => { if (dragPlayable && dragCard!.effect !== 'damage') match.playCard(dragCard!.instanceId); setDragId(null) }
  const dropToTarget = (targetRef: string) => { if (dragPlayable && dragCard!.effect === 'damage') match.playCard(dragCard!.instanceId, targetRef); setDragId(null) }

  // Click a hand card to inspect it (drag it to play). The detail panel's Play
  // button is the non-drag fallback.
  const clickHandCard = (c: HandCard) => setInspect({ card: c })
  const playFromDetail = (c: HandCard) => {
    if (!yourTurn || c.cost > you.juice || over) return
    if (c.effect === 'damage') setSel({ kind: 'card', id: c.instanceId })
    else match.playCard(c.instanceId)
    setInspect(null)
  }
  const clickMyUnit = (u: BoardUnit) => {
    setInspect({ unit: u })
    if (yourTurn && u.ready) {
      setSel((prev) => (prev?.kind === 'attacker' && prev.id === u.instanceId ? null : { kind: 'attacker', id: u.instanceId }))
    }
  }
  const resolveOn = (targetRef: string) => {
    if (!sel) return
    if (sel.kind === 'attacker') match.attack(sel.id, targetRef)
    else match.playCard(sel.id, targetRef)
    setSel(null)
  }
  const clickEnemyUnit = (u: BoardUnit) => {
    if (targeting) resolveOn(`op:${u.instanceId}`)
    else setInspect({ unit: u })
  }
  // A target is legal to click now: Spotlight forces attacks onto taunts.
  const targetableEnemyUnit = (u: BoardUnit) => targeting && (!enemyHasSpotlight || hasSpotlight(u) || sel?.kind === 'card')
  const targetableFace = targeting && (sel?.kind === 'card' || !enemyHasSpotlight)

  return (
    <section className="match" aria-labelledby="match-title">
      <header className="match__bar">
        <h1 id="match-title" className="match__title">Match</h1>
        <div className="match__modes" role="tablist" aria-label="Match mode">
          {(['online', 'practice'] as MatchMode[]).map((m) => (
            <button
              key={m}
              type="button"
              role="tab"
              aria-selected={match.mode === m}
              className={match.mode === m ? 'match__mode match__mode--on' : 'match__mode'}
              onClick={() => match.setMode(m)}
            >
              {m === 'online' ? 'Online' : 'Practice'}
            </button>
          ))}
        </div>
        <span className={`match__status match__status--${match.status}`} aria-live="polite">
          {statusLabel(match.status, match.pending)}
        </span>
      </header>

      {mission ? (
        <div className="match__mission" role="status">
          {mission.missionName} — vs {mission.bossName}
          <span className="match__mission-tier"> · {mission.difficultyTier}</span>
        </div>
      ) : null}

      {/* Opponent zone */}
      <BossBar
        seat="Opponent"
        s={opp}
        face={targetableFace}
        onFace={() => resolveOn(`boss:${foe}`)}
        dropOk={!!dragPlayable && dragCard!.effect === 'damage'}
        onDropCard={() => dropToTarget(`boss:${foe}`)}
      />
      <div className={`board board--enemy${targeting ? ' board--targeting' : ''}`} aria-label="Opponent's Operators">
        {opp.board.map((u) => (
          <Unit
            key={u.instanceId}
            u={u}
            target={targetableEnemyUnit(u)}
            dropOk={!!dragPlayable && dragCard!.effect === 'damage'}
            onDropCard={() => dropToTarget(`op:${u.instanceId}`)}
            onClick={() => clickEnemyUnit(u)}
          />
        ))}
        {opp.board.length === 0 ? <span className="match__hint">No enemy Operators</span> : null}
      </div>

      <div className="match__mid">{yourTurn ? 'Your turn' : over ? '' : 'Opponent…'}</div>

      {/* Your zone — drop a summon/utility card here to play it */}
      <div
        className={`board board--you${dragPlayable && dragCard!.effect !== 'damage' ? ' board--drop' : ''}`}
        aria-label="Your Operators"
        onDragOver={allowDrop(!!dragPlayable && dragCard!.effect !== 'damage')}
        onDrop={dropToBoard}
      >
        {you.board.map((u) => (
          <Unit key={u.instanceId} u={u} selected={sel?.kind === 'attacker' && sel.id === u.instanceId} armed={yourTurn && u.ready} onClick={() => clickMyUnit(u)} />
        ))}
        {you.board.length === 0 ? <span className="match__hint">No Operators in play — play one from your hand</span> : null}
      </div>
      <BossBar seat="You" s={you} />

      {/* Hand */}
      <div className="match__hand" aria-label="Your hand">
        {you.hand.map((c) => (
          <button
            key={c.instanceId}
            type="button"
            className={`handcard${sel?.kind === 'card' && sel.id === c.instanceId ? ' handcard--sel' : ''}${dragId === c.instanceId ? ' handcard--drag' : ''}`}
            disabled={over}
            draggable={!over}
            onDragStart={() => { setDragId(c.instanceId); setInspect({ card: c }) }}
            onDragEnd={() => setDragId(null)}
            onClick={() => clickHandCard(c)}
          >
            <span className="handcard__art" style={{ backgroundImage: `url(/assets/cards/${c.cardId}.webp)` }} aria-hidden="true" />
            <span className={`handcard__cost${c.cost > you.juice ? ' handcard__cost--short' : ''}`}>{c.cost}</span>
            <span className="handcard__name">{c.name}</span>
            <span className="handcard__text">{cardText(c)}</span>
          </button>
        ))}
        {you.hand.length === 0 ? <span className="match__hint">Hand empty — end your turn to draw</span> : null}
      </div>

      {targeting ? (
        <div className="match__targeting" role="status">
          Pick a target{enemyHasSpotlight && sel?.kind === 'attacker' ? ' (Spotlight — must hit the taunt)' : ''} · <button type="button" className="match__cancel" onClick={() => setSel(null)}>cancel</button>
        </div>
      ) : null}

      {/* Detail panel */}
      {inspect ? (
        <Detail
          inspect={inspect}
          onClose={() => setInspect(null)}
          onPlay={inspect.card && yourTurn && inspect.card.cost <= you.juice && !over ? () => playFromDetail(inspect.card!) : undefined}
          playLabel={inspect.card?.effect === 'damage' ? 'Play → pick target' : 'Play'}
        />
      ) : null}

      {match.correction ? (
        <div className="match__correction" role="alert">
          <span>{match.correction}</span>
          <button type="button" className="match__correction-x" aria-label="Dismiss" onClick={match.dismissCorrection}>×</button>
        </div>
      ) : null}

      {over ? (
        <div className={`match__result match__result--${state.winner === me ? 'win' : 'loss'}`} role="status">
          <span className="match__result-title">{state.winner === me ? 'You run this block.' : 'Busted.'}</span>
          <span className="match__result-sub">{state.winner === me ? 'Victory' : 'Defeat'}</span>
          <button type="button" className="match__action match__action--primary" onClick={match.newMatch}>New match</button>
        </div>
      ) : null}

      <footer className="match__actions">
        <button type="button" className="match__action" disabled={!yourTurn} onClick={match.heroPower}>Boss Power (2 → face)</button>
        <button type="button" className="match__action" disabled={!yourTurn} onClick={() => { setSel(null); match.endTurn() }}>End turn</button>
        <button type="button" className="match__action match__action--danger" onClick={match.concede}>Concede</button>
      </footer>

      {/* Hidden canvas keeps the renderer wired for a future unified board. */}
      <canvas ref={match.canvasRef} className="match__canvas match__canvas--hidden" aria-hidden="true" />
    </section>
  )
}

/** A boss resource bar: HP, Heat, Juice. Optionally a face-attack / drop target. */
function BossBar({ seat, s, face, onFace, dropOk, onDropCard }: { seat: string; s: { bossName: string; bossHp: number; heat: number; juice: number; maxJuice: number }; face?: boolean; onFace?: () => void; dropOk?: boolean; onDropCard?: () => void }) {
  return (
    <div
      className={`bossbar${face || dropOk ? ' bossbar--target' : ''}`}
      onClick={face ? onFace : undefined}
      role={face ? 'button' : undefined}
      onDragOver={dropOk ? (e) => e.preventDefault() : undefined}
      onDrop={dropOk ? onDropCard : undefined}
    >
      <div className="bossbar__id">
        <span className="bossbar__seat">{seat}</span>
        <span className="bossbar__hp">♥ {s.bossHp}</span>
      </div>
      <div className="bossbar__meters">
        <span className="bossbar__heat">Heat {s.heat}/10</span>
        <span className="bossbar__juice">Juice {s.juice}/{s.maxJuice}</span>
      </div>
      {face ? <span className="bossbar__aim">⌖ attack</span> : null}
    </div>
  )
}

/** A board Operator chip. */
function Unit({ u, selected, armed, target, dropOk, onDropCard, onClick }: { u: BoardUnit; selected?: boolean; armed?: boolean; target?: boolean; dropOk?: boolean; onDropCard?: () => void; onClick: () => void }) {
  const cls = ['unit']
  if (armed) cls.push('unit--armed')
  if (selected) cls.push('unit--sel')
  if (target || dropOk) cls.push('unit--target')
  if (hasSpotlight(u)) cls.push('unit--spotlight')
  return (
    <button
      type="button"
      className={cls.join(' ')}
      onClick={onClick}
      title={u.keywords.join(', ')}
      onDragOver={dropOk ? (e) => e.preventDefault() : undefined}
      onDrop={dropOk ? onDropCard : undefined}
    >
      <span className="unit__art" style={{ backgroundImage: `url(/assets/cards/${u.cardId}.webp)` }} aria-hidden="true" />
      <span className="unit__name">{u.name}</span>
      <span className="unit__stats">{u.atk}/{u.hp}</span>
      {u.keywords.length ? <span className="unit__kw">{u.keywords[0]}</span> : null}
    </button>
  )
}

/** Card / unit detail panel. */
function Detail({ inspect, onClose, onPlay, playLabel }: { inspect: NonNullable<Inspected>; onClose: () => void; onPlay?: () => void; playLabel?: string }) {
  const c = inspect.card
  const u = inspect.unit
  const cardId = c?.cardId ?? u?.cardId ?? ''
  const name = c?.name ?? u?.name ?? ''
  const text = c?.text ?? (u ? `${u.atk}/${u.hp}${u.keywords.length ? ' · ' + u.keywords.join(', ') : ''}` : '')
  return (
    <aside className="detail" aria-label={`${name} details`}>
      <button type="button" className="detail__x" aria-label="Close" onClick={onClose}>×</button>
      <span className="detail__art" style={{ backgroundImage: `url(/assets/cards/${cardId}.webp)` }} aria-hidden="true" />
      <div className="detail__body">
        <div className="detail__head">
          <span className="detail__name">{name}</span>
          {c ? <span className="detail__cost">{c.cost}</span> : null}
        </div>
        <span className="detail__type">{c?.type ?? 'Operator'}{c && (c.atk != null) ? ` · ${c.atk}/${c.hp}` : ''}</span>
        <p className="detail__text">{text}</p>
        {onPlay ? <button type="button" className="detail__play" onClick={onPlay}>{playLabel ?? 'Play'}</button> : null}
      </div>
    </aside>
  )
}

/** One-line summary of what a hand card does, from its effect. */
function cardText(c: { effect: string; amount: number; atk?: number; hp?: number }): string {
  switch (c.effect) {
    case 'damage': return `Deal ${c.amount} to a target`
    case 'summon': return `Operator ${c.atk ?? 1}/${c.hp ?? 1}`
    case 'draw': return `Draw ${c.amount}`
    case 'juice': return `+${c.amount} Juice`
    case 'cool': return `Lower Heat ${c.amount}`
    default: return ''
  }
}

/** Human-readable connection status, annotated with any in-flight predictions. */
function statusLabel(status: ReturnType<typeof useMatch>['status'], pending: number): string {
  const base =
    status === 'practice' ? 'Practice'
    : status === 'open' ? 'Live'
    : status === 'connecting' ? 'Connecting…'
    : status === 'reconnecting' ? 'Reconnecting…'
    : 'Offline'
  return pending > 0 ? `${base} · ${pending} pending` : base
}
