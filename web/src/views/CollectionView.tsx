import { useMemo } from 'react'
import { useSession } from '../auth/SessionProvider'
import { useDeckbuilder, type CollectionFilters } from '../deck/useDeckbuilder'
import { copyCapFor, DECK_CLASSES, LEGAL_OUTFIT_SIZE, type LegalityIssue } from '../deck/legality'
import type { CardClass } from '../api/types'

/**
 * Collection & deckbuilder view.
 *
 * Backed by {@link useDeckbuilder}, it browses the player's owned collection
 * (with text / class / cost filters), edits a working deck (add/remove cards,
 * rename, pick the class it is built around), and saves through the
 * collection-deck-service. Deck legality is checked live client-side against the
 * SHARED Outfit rules mirrored from `crates/domain/src/outfit.rs`: an illegal
 * deck blocks the Save button and every violation is surfaced inline (a summary
 * banner plus a per-card note on the offending deck rows).
 *
 * The layout is mobile-first: a single scrolling column of stacked panes on a
 * phone, widening to a two-column collection / deck split on a tablet+ viewport.
 */
export default function CollectionView() {
  const { state } = useSession()
  const playerId =
    state.status === 'ready' && state.session.authenticated ? state.session.identity.subject : ''
  const db = useDeckbuilder(playerId)

  // Index issues by card so each deck row can carry its own inline message.
  const issuesByCard = useMemo(() => {
    const map = new Map<string, LegalityIssue[]>()
    for (const issue of db.legality.issues) {
      if (!issue.cardId) continue
      const list = map.get(issue.cardId) ?? []
      list.push(issue)
      map.set(issue.cardId, list)
    }
    return map
  }, [db.legality.issues])

  const sizeIssue = db.legality.issues.find((i) => i.code === 'size')

  if (db.status.phase === 'loading') {
    return (
      <section className="deck" aria-labelledby="deck-title">
        <h1 id="deck-title" className="deck__title">Collection &amp; Decks</h1>
        <p className="deck__status" role="status" aria-live="polite">Loading your collection…</p>
      </section>
    )
  }

  if (db.status.phase === 'error') {
    return (
      <section className="deck" aria-labelledby="deck-title">
        <h1 id="deck-title" className="deck__title">Collection &amp; Decks</h1>
        <p className="deck__error" role="alert">{db.status.message}</p>
        {db.status.retriable ? (
          <button type="button" className="deck__btn" onClick={db.reload}>Try again</button>
        ) : null}
      </section>
    )
  }

  return (
    <section className="deck" aria-labelledby="deck-title">
      <header className="deck__head">
        <h1 id="deck-title" className="deck__title">Collection &amp; Decks</h1>
        <label className="deck__field">
          <span className="deck__field-label">Deck</span>
          <select
            className="deck__select"
            value={db.selectedDeckId ?? ''}
            onChange={(e) => (e.target.value ? db.selectDeck(e.target.value) : db.newDeck())}
          >
            <option value="">＋ New deck</option>
            {db.decks.map((d) => (
              <option key={d.deckId} value={d.deckId}>{d.name || d.deckId}</option>
            ))}
          </select>
        </label>
      </header>

      <div className="deck__panes">
        {/* ── Collection browser ─────────────────────────────────────────── */}
        <div className="deck__pane deck__pane--collection" aria-labelledby="deck-collection-title">
          <h2 id="deck-collection-title" className="deck__pane-title">Collection</h2>
          <Filters filters={db.filters} onChange={db.setFilters} />
          <ul className="deck__cards" aria-label="Owned cards">
            {db.collection.map((row) => {
              // The most copies you can legally run: never more than you own,
              // never more than the card's copy cap (1 for a Legendary).
              const cap = Math.min(row.owned, copyCapFor(row.card))
              const full = row.inDeck >= cap
              return (
                <li key={row.card.cardId} className={`deck__card deck__card--${row.card.rarity}`}>
                  <button
                    type="button"
                    className="deck__card-btn"
                    onClick={() => db.addCard(row.card.cardId)}
                    disabled={full}
                    aria-label={`Add ${row.card.name}`}
                  >
                    <span className="deck__cost" aria-hidden="true">{row.card.cost}</span>
                    <span
                      className="deck__art"
                      aria-hidden="true"
                      style={row.card.art ? { backgroundImage: `url(${row.card.art})`, filter: row.card.artTint ? `hue-rotate(${row.card.artTint}deg)` : undefined } : undefined}
                    >
                      {!row.card.art ? row.card.name.charAt(0) : null}
                    </span>
                    <span className="deck__card-name">{row.card.name}</span>
                    <span className="deck__badges">
                      <span className={`deck__type deck__type--${(row.card.cardType || '').toString().toLowerCase()}`}>
                        {row.card.cardType}
                      </span>
                      <span className="deck__class-chip">{row.card.cardClass}</span>
                    </span>
                    {row.card.text ? <span className="deck__text">{row.card.text}</span> : null}
                    <span className="deck__foot">
                      {row.card.heat ? <span className="deck__heat">+{row.card.heat} HEAT</span> : null}
                      <span className="deck__owned">{row.inDeck}/{cap}</span>
                    </span>
                  </button>
                </li>
              )
            })}
            {db.collection.length === 0 ? (
              <li className="deck__empty">No cards match your filters.</li>
            ) : null}
          </ul>
        </div>

        {/* ── Working deck ───────────────────────────────────────────────── */}
        <div className="deck__pane deck__pane--builder" aria-labelledby="deck-builder-title">
          <h2 id="deck-builder-title" className="deck__pane-title">Deck</h2>

          <div className="deck__meta">
            <label className="deck__field">
              <span className="deck__field-label">Name</span>
              <input
                className="deck__input"
                value={db.deckName}
                onChange={(e) => db.setDeckName(e.target.value)}
                placeholder="Deck name"
              />
            </label>
            <label className="deck__field">
              <span className="deck__field-label">Class</span>
              <select
                className="deck__select"
                value={db.deckClass}
                onChange={(e) => db.setDeckClass(e.target.value as (typeof DECK_CLASSES)[number])}
              >
                {DECK_CLASSES.map((c) => (
                  <option key={c} value={c}>{c}</option>
                ))}
              </select>
            </label>
          </div>

          <div
            className={`deck__count deck__count--${db.legality.size === LEGAL_OUTFIT_SIZE ? 'ok' : 'off'}`}
            aria-live="polite"
          >
            {db.legality.size} / {LEGAL_OUTFIT_SIZE} cards
          </div>

          {sizeIssue ? (
            <p className="deck__issue" role="status">{sizeIssue.message}</p>
          ) : null}

          <ul className="deck__list" aria-label="Cards in deck">
            {db.deckRows.map((row) => {
              const issues = issuesByCard.get(row.card.cardId) ?? []
              return (
                <li key={row.card.cardId} className={issues.length ? 'deck__row deck__row--bad' : 'deck__row'}>
                  <button
                    type="button"
                    className="deck__row-btn"
                    onClick={() => db.removeCard(row.card.cardId)}
                    aria-label={`Remove one ${row.card.name}`}
                  >
                    <span className="deck__cost" aria-hidden="true">{row.card.cost}</span>
                    <span className="deck__card-name">{row.card.name}</span>
                    <span className="deck__count-badge">×{row.count}</span>
                  </button>
                  {issues.map((issue, i) => (
                    <p key={i} className="deck__issue deck__issue--card" role="alert">{issue.message}</p>
                  ))}
                </li>
              )
            })}
            {db.deckRows.length === 0 ? (
              <li className="deck__empty">Tap cards on the left to add them.</li>
            ) : null}
          </ul>

          <footer className="deck__actions">
            {db.saveError ? <p className="deck__error" role="alert">{db.saveError}</p> : null}
            {db.saved ? <p className="deck__saved" role="status">Deck saved.</p> : null}
            {!db.legality.legal ? (
              <p className="deck__blocked" role="status">
                Fix {db.legality.issues.length} issue{db.legality.issues.length === 1 ? '' : 's'} to save.
              </p>
            ) : null}
            <button
              type="button"
              className="deck__btn deck__btn--primary"
              onClick={db.save}
              disabled={!db.legality.legal || db.saving}
            >
              {db.saving ? 'Saving…' : 'Save deck'}
            </button>
          </footer>
        </div>
      </div>
    </section>
  )
}

/** The collection filter bar: text search, class, and a cost ceiling. */
function Filters({
  filters,
  onChange,
}: {
  filters: CollectionFilters
  onChange: (next: Partial<CollectionFilters>) => void
}) {
  const classes: readonly (CardClass | 'all')[] = ['all', 'neutral', ...DECK_CLASSES]
  return (
    <div className="deck__filters">
      <input
        className="deck__input"
        type="search"
        value={filters.text}
        onChange={(e) => onChange({ text: e.target.value })}
        placeholder="Search cards…"
        aria-label="Search cards"
      />
      <select
        className="deck__select"
        value={filters.cardClass}
        onChange={(e) => onChange({ cardClass: e.target.value as CardClass | 'all' })}
        aria-label="Filter by class"
      >
        {classes.map((c) => (
          <option key={c} value={c}>{c === 'all' ? 'All classes' : c}</option>
        ))}
      </select>
      <select
        className="deck__select"
        value={filters.maxCost ?? ''}
        onChange={(e) => onChange({ maxCost: e.target.value === '' ? null : Number(e.target.value) })}
        aria-label="Filter by maximum cost"
      >
        <option value="">Any cost</option>
        {[1, 2, 3, 4, 5, 6, 7].map((n) => (
          <option key={n} value={n}>≤ {n}</option>
        ))}
      </select>
    </div>
  )
}
