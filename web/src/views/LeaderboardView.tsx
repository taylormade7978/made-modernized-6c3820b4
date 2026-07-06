import { useSession } from '../auth/SessionProvider'
import { useLeaderboard } from '../leaderboard/useLeaderboard'

/**
 * Leaderboard / ranked-standings view.
 *
 * Renders the current season's ranked standings from the leaderboard-service via
 * {@link useLeaderboard}: the season id, a rank-ordered table of players (rank,
 * name, rating, stars), and prev/next paging derived from the server's total.
 * The signed-in player's own row is highlighted so they can spot their standing
 * at a glance.
 *
 * The layout is mobile-first: a single full-width standings list that stays
 * legible on a phone and simply gains breathing room on larger viewports.
 */
export default function LeaderboardView() {
  const { state } = useSession()
  const selfId =
    state.status === 'ready' && state.session.authenticated ? state.session.identity.subject : ''
  const board = useLeaderboard()

  if (board.status.phase === 'loading' && !board.page) {
    return (
      <section className="ranks" aria-labelledby="ranks-title">
        <h1 id="ranks-title" className="ranks__title">Leaderboard</h1>
        <p className="ranks__status" role="status" aria-live="polite">Loading standings…</p>
      </section>
    )
  }

  if (board.status.phase === 'error') {
    return (
      <section className="ranks" aria-labelledby="ranks-title">
        <h1 id="ranks-title" className="ranks__title">Leaderboard</h1>
        <p className="ranks__error" role="alert">{board.status.message}</p>
        {board.status.retriable ? (
          <button type="button" className="ranks__btn" onClick={board.reload}>Try again</button>
        ) : null}
      </section>
    )
  }

  const page = board.page
  if (!page) return null

  return (
    <section className="ranks" aria-labelledby="ranks-title">
      <header className="ranks__head">
        <h1 id="ranks-title" className="ranks__title">Leaderboard</h1>
        <p className="ranks__season">
          Season <span className="ranks__season-id">{page.seasonId}</span> ·{' '}
          {page.total} ranked player{page.total === 1 ? '' : 's'}
        </p>
      </header>

      <ol className="ranks__list" aria-label="Ranked standings">
        {page.entries.map((entry) => (
          <li
            key={entry.playerId}
            className={entry.playerId === selfId ? 'ranks__row ranks__row--self' : 'ranks__row'}
            aria-current={entry.playerId === selfId ? 'true' : undefined}
          >
            <span className="ranks__rank">#{entry.rank}</span>
            <span className="ranks__name">{entry.displayName}</span>
            <span className="ranks__stars" aria-label={`${entry.stars} stars`}>
              ★ {entry.stars}
            </span>
            <span className="ranks__rating">{entry.rating}</span>
          </li>
        ))}
        {page.entries.length === 0 ? (
          <li className="ranks__empty">No ranked players in this season yet.</li>
        ) : null}
      </ol>

      <footer className="ranks__pager">
        <button
          type="button"
          className="ranks__btn"
          onClick={board.prevPage}
          disabled={!board.hasPrev || board.status.phase === 'loading'}
        >
          ‹ Prev
        </button>
        <span className="ranks__page-label" aria-live="polite">Page {board.pageIndex + 1}</span>
        <button
          type="button"
          className="ranks__btn"
          onClick={board.nextPage}
          disabled={!board.hasNext || board.status.phase === 'loading'}
        >
          Next ›
        </button>
      </footer>
    </section>
  )
}
