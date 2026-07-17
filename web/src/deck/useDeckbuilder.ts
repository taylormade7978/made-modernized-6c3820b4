/**
 * `useDeckbuilder` — the React controller that backs the collection / deck view.
 *
 * It owns the whole deckbuilding workflow against the collection-deck-service:
 *
 *  - loads the player's {@link CollectionResponse} (owned cards + saved decks)
 *    and the {@link Card} catalog in parallel through the typed {@link api};
 *  - exposes a browsable/filterable view of the owned collection (text, class,
 *    and cost filters);
 *  - holds a mutable *working deck* the player edits (add/remove cards, rename,
 *    pick the class it is built around);
 *  - recomputes {@link checkLegality} live on every edit, using the SHARED rules
 *    mirrored from `crates/domain/src/outfit.rs`, so the Save button can be
 *    blocked and per-card issues surfaced inline; and
 *  - persists a legal deck via `PUT /v1/collection/{playerId}/decks/{deckId}`,
 *    folding the server's returned {@link Deck} back into the local deck list.
 *
 * The legality gate is defensive-in-depth: an illegal deck is never sent, and
 * the server's `SaveOutfitCmd` re-validates the identical invariants server-side.
 */
import { useCallback, useEffect, useMemo, useState } from 'react'
import { api, ApiError } from '../api'
import type { Card, CardClass, CollectionResponse, Deck, OwnedCard } from '../api/types'
import { subscribe } from '../api/realtime'
import {
  checkLegality,
  countCards,
  DECK_CLASSES,
  inferDeckClass,
  type DeckClass,
  type LegalityReport,
} from './legality'

/** Filters applied to the owned-collection browser. */
export interface CollectionFilters {
  /** Case-insensitive substring match on card name (empty ⇒ no text filter). */
  readonly text: string
  /** Restrict to a single class, or `all` for every class. */
  readonly cardClass: CardClass | 'all'
  /** Only cards with `cost <= maxCost`, or `null` for no cost ceiling. */
  readonly maxCost: number | null
}

const DEFAULT_FILTERS: CollectionFilters = { text: '', cardClass: 'all', maxCost: null }

/** A row in the browsable collection: the definition joined to owned quantity. */
export interface CollectionRow {
  readonly card: Card
  readonly owned: number
  /** How many copies are already in the working deck (for "2 / 2" badges). */
  readonly inDeck: number
}

/** A row in the working deck: the definition plus how many copies are included. */
export interface DeckRow {
  readonly card: Card
  readonly count: number
}

/** Async loading lifecycle for the collection + catalog fetch. */
export type LoadStatus =
  | { readonly phase: 'loading' }
  | { readonly phase: 'ready' }
  | { readonly phase: 'error'; readonly message: string; readonly retriable: boolean }

/** Everything the collection/deck view needs to render and drive deckbuilding. */
export interface DeckbuilderController {
  readonly status: LoadStatus
  /** The player's saved decks (updated in place as decks are saved). */
  readonly decks: readonly Deck[]
  /** The id of the deck currently being edited, or `null` for an unsaved draft. */
  readonly selectedDeckId: string | null
  /** The browsable, filtered owned collection. */
  readonly collection: readonly CollectionRow[]
  /** The working deck's rows (distinct cards with counts), in insertion order. */
  readonly deckRows: readonly DeckRow[]
  readonly deckName: string
  readonly deckClass: DeckClass
  /** Live legality of the working deck (drives inline messaging + Save gating). */
  readonly legality: LegalityReport
  readonly filters: CollectionFilters
  /** Whether a save is currently in flight. */
  readonly saving: boolean
  /** The last save error, or `null`. */
  readonly saveError: string | null
  /** Whether the last save succeeded (drives a transient confirmation). */
  readonly saved: boolean
  readonly setFilters: (next: Partial<CollectionFilters>) => void
  readonly selectDeck: (deckId: string) => void
  readonly newDeck: () => void
  readonly setDeckName: (name: string) => void
  readonly setDeckClass: (deckClass: DeckClass) => void
  readonly addCard: (cardId: string) => void
  readonly removeCard: (cardId: string) => void
  readonly save: () => void
  readonly reload: () => void
}

/** Generate a client-side deck id for a brand-new draft. */
function newDeckId(): string {
  const rand = Math.random().toString(36).slice(2, 8)
  return `deck-${rand}`
}

/**
 * Drive the collection/deck view for `playerId`. When `playerId` is empty (no
 * resolved session yet) the controller stays in its loading phase and issues no
 * requests, so the caller can render it unconditionally under the session gate.
 */
export function useDeckbuilder(playerId: string): DeckbuilderController {
  const [status, setStatus] = useState<LoadStatus>({ phase: 'loading' })
  const [cards, setCards] = useState<ReadonlyMap<string, Card>>(new Map())
  const [owned, setOwned] = useState<readonly OwnedCard[]>([])
  const [decks, setDecks] = useState<readonly Deck[]>([])

  const [selectedDeckId, setSelectedDeckId] = useState<string | null>(null)
  const [deckName, setDeckName] = useState('New deck')
  const [deckClass, setDeckClass] = useState<DeckClass>(DECK_CLASSES[0])
  const [cardIds, setCardIds] = useState<readonly string[]>([])

  const [filters, setFiltersState] = useState<CollectionFilters>(DEFAULT_FILTERS)
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)

  // A monotonically bumped nonce to force a reload on demand.
  const [reloadNonce, setReloadNonce] = useState(0)
  const reload = useCallback(() => setReloadNonce((n) => n + 1), [])

  // Live updates: subscribe to the player's collection and re-pull on any
  // server-pushed change (a deck save from this or another device). The trigger
  // is the push, not a timer — no polling.
  useEffect(() => {
    if (!playerId) return
    return subscribe<{ collectionChanged: unknown }>(
      `subscription($p:ID!){ collectionChanged(playerId:$p){ playerId } }`,
      { p: playerId },
      () => reload(),
    )
  }, [playerId, reload])

  // Load owned collection + card catalog together.
  useEffect(() => {
    if (!playerId) return
    const ctrl = new AbortController()
    setStatus({ phase: 'loading' })
    Promise.all([
      api.collection.get(playerId, { signal: ctrl.signal }),
      api.catalog.listCards({ signal: ctrl.signal }),
    ])
      .then(([collection, catalog]: [CollectionResponse, readonly Card[]]) => {
        if (ctrl.signal.aborted) return
        setCards(new Map(catalog.map((c) => [c.cardId, c])))
        setOwned(collection.ownedCards)
        setDecks(collection.decks)
        setStatus({ phase: 'ready' })
      })
      .catch((err: unknown) => {
        if (ctrl.signal.aborted) return
        const e = err instanceof ApiError ? err : null
        setStatus({
          phase: 'error',
          message: e?.message ?? 'Failed to load your collection.',
          retriable: e?.retriable ?? true,
        })
      })
    return () => ctrl.abort()
  }, [playerId, reloadNonce])

  const ownedMap = useMemo(
    () => new Map(owned.map((o) => [o.cardId, o.quantity])),
    [owned],
  )

  // Live legality of the working deck against the shared Outfit invariants.
  const legality = useMemo(
    () => checkLegality(cardIds, { deckClass, cards, owned: ownedMap }),
    [cardIds, deckClass, cards, ownedMap],
  )

  const deckCounts = useMemo(() => countCards(cardIds), [cardIds])

  // The browsable, filtered collection: every owned card joined to its
  // definition, narrowed by the active filters and sorted by cost then name.
  const collection = useMemo<CollectionRow[]>(() => {
    const text = filters.text.trim().toLowerCase()
    const rows: CollectionRow[] = []
    for (const o of owned) {
      const card = cards.get(o.cardId)
      if (!card) continue
      if (filters.cardClass !== 'all' && card.cardClass !== filters.cardClass) continue
      if (filters.maxCost !== null && card.cost > filters.maxCost) continue
      if (text && !card.name.toLowerCase().includes(text)) continue
      rows.push({ card, owned: o.quantity, inDeck: deckCounts.get(o.cardId) ?? 0 })
    }
    rows.sort((a, b) => a.card.cost - b.card.cost || a.card.name.localeCompare(b.card.name))
    return rows
  }, [owned, cards, filters, deckCounts])

  // Working-deck rows: distinct cards with counts, in first-added order.
  const deckRows = useMemo<DeckRow[]>(() => {
    const rows: DeckRow[] = []
    for (const [cardId, count] of deckCounts) {
      const card = cards.get(cardId)
      if (card) rows.push({ card, count })
    }
    return rows
  }, [deckCounts, cards])

  const setFilters = useCallback((next: Partial<CollectionFilters>) => {
    setFiltersState((prev) => ({ ...prev, ...next }))
  }, [])

  // Reset the transient save feedback whenever the working deck changes.
  const clearSaveFeedback = useCallback(() => {
    setSaved(false)
    setSaveError(null)
  }, [])

  const selectDeck = useCallback(
    (deckId: string) => {
      const deck = decks.find((d) => d.deckId === deckId)
      if (!deck) return
      setSelectedDeckId(deck.deckId)
      setDeckName(deck.name)
      setCardIds(deck.cardIds)
      setDeckClass(inferDeckClass(deck.cardIds, cards) ?? DECK_CLASSES[0])
      clearSaveFeedback()
    },
    [decks, cards, clearSaveFeedback],
  )

  const newDeck = useCallback(() => {
    setSelectedDeckId(null)
    setDeckName('New deck')
    setDeckClass(DECK_CLASSES[0])
    setCardIds([])
    clearSaveFeedback()
  }, [clearSaveFeedback])

  const addCard = useCallback(
    (cardId: string) => {
      clearSaveFeedback()
      setCardIds((prev) => [...prev, cardId])
    },
    [clearSaveFeedback],
  )

  const removeCard = useCallback(
    (cardId: string) => {
      clearSaveFeedback()
      // Remove a single copy (the last one added), leaving other copies intact.
      setCardIds((prev) => {
        const idx = prev.lastIndexOf(cardId)
        if (idx < 0) return prev
        return [...prev.slice(0, idx), ...prev.slice(idx + 1)]
      })
    },
    [clearSaveFeedback],
  )

  const save = useCallback(() => {
    // Client-side gate: never send an illegal deck (the server re-checks too).
    if (!legality.legal || !playerId) return
    const deckId = selectedDeckId ?? newDeckId()
    setSaving(true)
    setSaveError(null)
    setSaved(false)
    api.collection
      .saveDeck(playerId, deckId, { name: deckName.trim() || 'Untitled deck', cardIds })
      .then((saved: Deck) => {
        // Upsert the returned deck into the local list and keep editing it.
        setDecks((prev) => {
          const without = prev.filter((d) => d.deckId !== saved.deckId)
          return [...without, saved]
        })
        setSelectedDeckId(saved.deckId)
        setSaved(true)
      })
      .catch((err: unknown) => {
        const e = err instanceof ApiError ? err : null
        setSaveError(e?.message ?? 'Failed to save the deck.')
      })
      .finally(() => setSaving(false))
  }, [legality.legal, playerId, selectedDeckId, deckName, cardIds])

  // Keep the setters that also clear save feedback wrapped consistently.
  const setDeckNameCb = useCallback(
    (name: string) => {
      clearSaveFeedback()
      setDeckName(name)
    },
    [clearSaveFeedback],
  )
  const setDeckClassCb = useCallback(
    (next: DeckClass) => {
      clearSaveFeedback()
      setDeckClass(next)
    },
    [clearSaveFeedback],
  )

  return useMemo(
    () => ({
      status,
      decks,
      selectedDeckId,
      collection,
      deckRows,
      deckName,
      deckClass,
      legality,
      filters,
      saving,
      saveError,
      saved,
      setFilters,
      selectDeck,
      newDeck,
      setDeckName: setDeckNameCb,
      setDeckClass: setDeckClassCb,
      addCard,
      removeCard,
      save,
      reload,
    }),
    [
      status,
      decks,
      selectedDeckId,
      collection,
      deckRows,
      deckName,
      deckClass,
      legality,
      filters,
      saving,
      saveError,
      saved,
      setFilters,
      selectDeck,
      newDeck,
      setDeckNameCb,
      setDeckClassCb,
      addCard,
      removeCard,
      save,
      reload,
    ],
  )
}
