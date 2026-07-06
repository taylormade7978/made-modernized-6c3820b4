import type { DifficultyTier } from '../api/types'

/**
 * Router-state payload the story view hands to the match view when a
 * MissionAttempt is launched against the AI-opponent service.
 *
 * The story view routes to `/match` with `{ state: { mission: MissionLaunch } }`;
 * the match view reads it (see `MatchView`) and passes the `ticket` to
 * {@link useMatch}, so the realtime socket joins the authoritative match the AI
 * opponent is already seated in. The remaining fields drive a "you're fighting
 * <boss> on <tier>" banner over the board.
 */
export interface MissionLaunch {
  /** The matchmaking ticket returned by the launched MissionAttempt. */
  readonly ticket: string
  readonly missionName: string
  readonly bossName: string
  readonly difficultyTier: DifficultyTier
}
