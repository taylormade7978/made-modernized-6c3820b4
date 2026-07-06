/**
 * `useStory` — the React controller behind the story / mission-entry view.
 *
 * It loads the player's campaign (missions joined to their boss and bound
 * {@link AIProfile}) from the story service, then launches a **MissionAttempt
 * against the AI-opponent service** on demand. A launch is a `POST` that seats
 * the AI opponent in an authoritative match and returns the {@link
 * MissionAttempt} with the `matchTicket` the caller hands to the realtime
 * socket (the match view then plays it). The hook stays navigation-agnostic:
 * `launch()` takes an `onLaunched` callback the view uses to route into the
 * match, so this controller never imports the router.
 */
import { useCallback, useEffect, useMemo, useState } from 'react'
import { api, ApiError } from '../api'
import type { Mission, MissionAttempt } from '../api/types'

/** Async loading lifecycle for the campaign fetch. */
export type LoadStatus =
  | { readonly phase: 'loading' }
  | { readonly phase: 'ready' }
  | { readonly phase: 'error'; readonly message: string; readonly retriable: boolean }

/** Everything the story view needs to list missions and launch an attempt. */
export interface StoryController {
  readonly status: LoadStatus
  /** The player's campaign missions, in service order. */
  readonly missions: readonly Mission[]
  /** The mission id whose attempt is currently being launched, or `null`. */
  readonly launchingMissionId: string | null
  /** The last launch error, or `null`. */
  readonly launchError: string | null
  /**
   * Launch a MissionAttempt against the AI opponent, invoking `onLaunched` with
   * the created attempt (and its `matchTicket`) on success so the view can route
   * into the match. A locked mission, or an in-flight launch, is a no-op.
   */
  readonly launch: (mission: Mission, onLaunched: (attempt: MissionAttempt) => void) => void
  readonly reload: () => void
}

/**
 * Drive the story view for `playerId`. When `playerId` is empty (no resolved
 * session yet) the controller stays in its loading phase and issues no request,
 * so the caller can render it unconditionally under the session gate.
 */
export function useStory(playerId: string): StoryController {
  const [status, setStatus] = useState<LoadStatus>({ phase: 'loading' })
  const [missions, setMissions] = useState<readonly Mission[]>([])
  const [launchingMissionId, setLaunchingMissionId] = useState<string | null>(null)
  const [launchError, setLaunchError] = useState<string | null>(null)

  const [reloadNonce, setReloadNonce] = useState(0)
  const reload = useCallback(() => setReloadNonce((n) => n + 1), [])

  useEffect(() => {
    if (!playerId) return
    const ctrl = new AbortController()
    setStatus({ phase: 'loading' })
    api.story
      .listMissions(playerId, { signal: ctrl.signal })
      .then((response) => {
        if (ctrl.signal.aborted) return
        setMissions(response.missions)
        setStatus({ phase: 'ready' })
      })
      .catch((err: unknown) => {
        if (ctrl.signal.aborted) return
        const e = err instanceof ApiError ? err : null
        setStatus({
          phase: 'error',
          message: e?.message ?? 'Failed to load the story campaign.',
          retriable: e?.retriable ?? true,
        })
      })
    return () => ctrl.abort()
  }, [playerId, reloadNonce])

  const launch = useCallback(
    (mission: Mission, onLaunched: (attempt: MissionAttempt) => void) => {
      if (!playerId || !mission.unlocked || launchingMissionId) return
      setLaunchingMissionId(mission.missionId)
      setLaunchError(null)
      api.story
        .launchAttempt(playerId, mission.missionId, { playerId, missionId: mission.missionId })
        .then((attempt: MissionAttempt) => {
          onLaunched(attempt)
        })
        .catch((err: unknown) => {
          const e = err instanceof ApiError ? err : null
          setLaunchError(e?.message ?? 'Failed to start the mission.')
        })
        .finally(() => setLaunchingMissionId(null))
    },
    [playerId, launchingMissionId],
  )

  return useMemo(
    () => ({ status, missions, launchingMissionId, launchError, launch, reload }),
    [status, missions, launchingMissionId, launchError, launch, reload],
  )
}
