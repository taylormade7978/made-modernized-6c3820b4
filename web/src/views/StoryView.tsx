import { useNavigate } from 'react-router-dom'
import { useSession } from '../auth/SessionProvider'
import { useStory } from '../story/useStory'
import type { DifficultyTier, Mission } from '../api/types'
import type { MissionLaunch } from '../match/mission'

/**
 * Story / mission-entry view.
 *
 * Lists the single-player campaign — each mission with its boss and the AI
 * profile (difficulty tier + strategy) it is fought against — from the story
 * service via {@link useStory}. Choosing **Play** launches a MissionAttempt
 * against the AI-opponent service and, on success, routes into the match view
 * carrying the returned `matchTicket` (as router state), which seats the player
 * in the authoritative match the AI opponent is already in.
 *
 * The layout is mobile-first: a single scrolling column of mission cards on a
 * phone, widening to a responsive grid on a tablet+ viewport.
 */
export default function StoryView() {
  const { state } = useSession()
  const playerId =
    state.status === 'ready' && state.session.authenticated ? state.session.identity.subject : ''
  const story = useStory(playerId)
  const navigate = useNavigate()

  if (story.status.phase === 'loading') {
    return (
      <section className="story" aria-labelledby="story-title">
        <h1 id="story-title" className="story__title">Story</h1>
        <p className="story__status" role="status" aria-live="polite">Loading the campaign…</p>
      </section>
    )
  }

  if (story.status.phase === 'error') {
    return (
      <section className="story" aria-labelledby="story-title">
        <h1 id="story-title" className="story__title">Story</h1>
        <p className="story__error" role="alert">{story.status.message}</p>
        {story.status.retriable ? (
          <button type="button" className="story__btn" onClick={story.reload}>Try again</button>
        ) : null}
      </section>
    )
  }

  const play = (mission: Mission) =>
    story.launch(mission, (attempt) => {
      // Hand the mission's match ticket to the match view; it joins the
      // authoritative match the AI opponent service is seated in.
      const launch: MissionLaunch = {
        ticket: attempt.matchTicket,
        missionName: mission.name,
        bossName: mission.boss.name,
        difficultyTier: attempt.difficultyTier,
      }
      navigate('/match', { state: { mission: launch } })
    })

  return (
    <section className="story" aria-labelledby="story-title">
      <h1 id="story-title" className="story__title">Story</h1>

      {story.launchError ? (
        <p className="story__error" role="alert">{story.launchError}</p>
      ) : null}

      <ul className="story__grid" aria-label="Missions">
        {story.missions.map((mission) => {
          const launching = story.launchingMissionId === mission.missionId
          return (
            <li
              key={mission.missionId}
              className={mission.unlocked ? 'story__card' : 'story__card story__card--locked'}
            >
              <div className="story__card-head">
                <span className={`story__tier story__tier--${mission.difficultyTier.toLowerCase()}`}>
                  {tierLabel(mission.difficultyTier)}
                </span>
                {mission.firstClearRewardClaimed ? (
                  <span className="story__cleared" aria-label="Cleared">✓ Cleared</span>
                ) : null}
              </div>
              <h2 className="story__mission-name">{mission.name}</h2>
              <p className="story__mission-desc">{mission.description}</p>
              <dl className="story__boss">
                <dt className="story__boss-label">Boss</dt>
                <dd className="story__boss-name">
                  {mission.boss.name}
                  <span className="story__boss-hp"> · {mission.boss.startingHp} HP</span>
                </dd>
                <dt className="story__boss-label">Opponent</dt>
                <dd className="story__ai">
                  {strategyLabel(mission.aiProfile.strategyKind)}
                  {mission.aiProfile.mctsBudget > 0 ? (
                    <span className="story__ai-budget"> · {mission.aiProfile.mctsBudget} sims</span>
                  ) : null}
                </dd>
              </dl>
              <button
                type="button"
                className="story__btn story__btn--primary"
                onClick={() => play(mission)}
                disabled={!mission.unlocked || story.launchingMissionId !== null}
                aria-label={`Play ${mission.name}`}
              >
                {launching ? 'Starting…' : mission.unlocked ? 'Play' : 'Locked'}
              </button>
            </li>
          )
        })}
        {story.missions.length === 0 ? (
          <li className="story__empty">No missions available yet.</li>
        ) : null}
      </ul>
    </section>
  )
}

/** Human label for a difficulty tier. */
function tierLabel(tier: DifficultyTier): string {
  return tier
}

/** Human label for an AI strategy kind. */
function strategyLabel(kind: Mission['aiProfile']['strategyKind']): string {
  return kind === 'Scripted' ? 'Scripted AI' : 'Search AI'
}
