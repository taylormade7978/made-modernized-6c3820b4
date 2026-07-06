import { useLocation } from 'react-router-dom'
import { useMatch, type MatchMode } from '../match/useMatch'
import type { MissionLaunch } from '../match/mission'

/**
 * Match / board view — the primary play surface.
 *
 * It renders the authoritative board on an HTML5 Canvas and drives it through
 * {@link useMatch}, which runs the shared rules for optimistic prediction, sends
 * actions to the authoritative WebSocket server, and reconciles (rolling back a
 * mispredicted move against server truth). A **Practice** toggle detaches the
 * socket and runs a whole match fully client-side against the same rules.
 *
 * When reached from the story view (via router state carrying a
 * {@link MissionLaunch}), the match joins the AI opponent's authoritative match
 * with the launched attempt's `ticket`, and a banner names the boss being fought.
 *
 * The layout is mobile-first: a fixed header + action bar bracket a flex-filling
 * canvas, so the board scales to the viewport and the controls stay reachable
 * with a thumb.
 */
export default function MatchView() {
  const location = useLocation()
  const mission = (location.state as { mission?: MissionLaunch } | null)?.mission
  const match = useMatch('live', mission?.ticket)
  const { state } = match
  const yourTurn = state.turn === match.selfSeat && state.phase === 'active'
  const over = state.phase === 'completed'

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

      <div className="match__stage">
        {mission ? (
          <div className="match__mission" role="status">
            {mission.missionName} — vs {mission.bossName}
            <span className="match__mission-tier"> · {mission.difficultyTier}</span>
          </div>
        ) : null}
        <canvas ref={match.canvasRef} className="match__canvas" aria-label="Game board" role="img" />
        {match.correction ? (
          <div className="match__correction" role="alert">
            <span>{match.correction}</span>
            <button type="button" className="match__correction-x" aria-label="Dismiss" onClick={match.dismissCorrection}>
              ×
            </button>
          </div>
        ) : null}
      </div>

      <footer className="match__actions">
        <button type="button" className="match__action" disabled={!yourTurn} onClick={match.playCard}>
          Play card
        </button>
        <button type="button" className="match__action" disabled={!yourTurn} onClick={match.heroPower}>
          Hero power
        </button>
        <button type="button" className="match__action" disabled={!yourTurn} onClick={match.endTurn}>
          End turn
        </button>
        {over ? (
          <button type="button" className="match__action match__action--primary" onClick={match.newMatch}>
            New match
          </button>
        ) : (
          <button type="button" className="match__action match__action--danger" onClick={match.concede}>
            Concede
          </button>
        )}
      </footer>
    </section>
  )
}

/** Human-readable connection status, annotated with any in-flight predictions. */
function statusLabel(status: ReturnType<typeof useMatch>['status'], pending: number): string {
  const base =
    status === 'practice'
      ? 'Practice'
      : status === 'open'
        ? 'Live'
        : status === 'connecting'
          ? 'Connecting…'
          : status === 'reconnecting'
            ? 'Reconnecting…'
            : 'Offline'
  return pending > 0 ? `${base} · ${pending} pending` : base
}
