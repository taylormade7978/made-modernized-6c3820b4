/**
 * `useLeaderboard` — the React controller behind the ranked-standings view.
 *
 * It pages the season leaderboard from the leaderboard-service through the typed
 * {@link api}, exposing the current {@link LeaderboardPage} (season id + ranked
 * rows) plus simple `nextPage` / `prevPage` navigation. Paging is derived from
 * the server's `total` / `pageSize`, so the view never has to do arithmetic.
 *
 * An optional `seasonId` pins the view to a specific season; omitting it lets
 * the service resolve the current season and echo it back in the response.
 */
import { useCallback, useEffect, useMemo, useState } from 'react'
import { api, ApiError } from '../api'
import type { LeaderboardPage } from '../api/types'

/** Default rows per page — mirrors the service's page size. */
const DEFAULT_PAGE_SIZE = 20

/** Async loading lifecycle for a leaderboard page fetch. */
export type LoadStatus =
  | { readonly phase: 'loading' }
  | { readonly phase: 'ready' }
  | { readonly phase: 'error'; readonly message: string; readonly retriable: boolean }

/** Everything the leaderboard view needs to render standings and page through them. */
export interface LeaderboardController {
  readonly status: LoadStatus
  /** The most recently loaded page, or `null` before the first load resolves. */
  readonly page: LeaderboardPage | null
  /** The zero-based page index currently requested. */
  readonly pageIndex: number
  /** Whether a further page of standings exists after this one. */
  readonly hasNext: boolean
  /** Whether an earlier page exists before this one. */
  readonly hasPrev: boolean
  readonly nextPage: () => void
  readonly prevPage: () => void
  readonly reload: () => void
}

/**
 * Drive the leaderboard view. `seasonId` optionally pins a season; when omitted
 * the service resolves the active season and returns its id on the page.
 */
export function useLeaderboard(seasonId?: string): LeaderboardController {
  const [status, setStatus] = useState<LoadStatus>({ phase: 'loading' })
  const [page, setPage] = useState<LeaderboardPage | null>(null)
  const [pageIndex, setPageIndex] = useState(0)

  const [reloadNonce, setReloadNonce] = useState(0)
  const reload = useCallback(() => setReloadNonce((n) => n + 1), [])

  // Reset to the first page whenever the pinned season changes.
  useEffect(() => setPageIndex(0), [seasonId])

  useEffect(() => {
    const ctrl = new AbortController()
    setStatus({ phase: 'loading' })
    api.leaderboard
      .list({ seasonId, page: pageIndex, pageSize: DEFAULT_PAGE_SIZE }, { signal: ctrl.signal })
      .then((result: LeaderboardPage) => {
        if (ctrl.signal.aborted) return
        setPage(result)
        setStatus({ phase: 'ready' })
      })
      .catch((err: unknown) => {
        if (ctrl.signal.aborted) return
        const e = err instanceof ApiError ? err : null
        setStatus({
          phase: 'error',
          message: e?.message ?? 'Failed to load the leaderboard.',
          retriable: e?.retriable ?? true,
        })
      })
    return () => ctrl.abort()
  }, [seasonId, pageIndex, reloadNonce])

  // A next page exists when we have not yet covered `total` ranked players.
  const hasNext = useMemo(() => {
    if (!page) return false
    return (page.page + 1) * page.pageSize < page.total
  }, [page])
  const hasPrev = pageIndex > 0

  const nextPage = useCallback(() => setPageIndex((i) => i + 1), [])
  const prevPage = useCallback(() => setPageIndex((i) => Math.max(0, i - 1)), [])

  return useMemo(
    () => ({ status, page, pageIndex, hasNext, hasPrev, nextPage, prevPage, reload }),
    [status, page, pageIndex, hasNext, hasPrev, nextPage, prevPage, reload],
  )
}
