/**
 * API client layer tests, driven against a mocked network (MSW).
 *
 * Coverage:
 *  - typed happy-path calls decode into the expected DTOs,
 *  - request bodies/methods are sent as declared,
 *  - a 401 fires the injected login-redirect hook and is not retried,
 *  - backend error envelopes normalize into a single {@link ApiError} shape,
 *  - transient failures (5xx) are retried with (stubbed) backoff, then succeed,
 *  - transport failures normalize to `kind: 'network'`,
 *  - a disabled capability short-circuits before any network call, and
 *  - the realtime WebSocket handshake URL is built correctly.
 */
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest'
import { http, HttpResponse } from 'msw'
import { setupServer } from 'msw/node'

import { createApiClient, type ApiClient } from './client'
import { createHttpClient } from './http'
import { ApiError } from './errors'
import type { ApiConfig } from '../config/api'

const BASE = 'https://api.testnet.made.vforce360.ai/v1'

const testConfig: ApiConfig = {
  env: 'testnet',
  project: 'made',
  restBaseUrl: BASE,
  wsBaseUrl: 'wss://ws.testnet.made.vforce360.ai',
  capabilities: { collection: true, leaderboard: true, shop: true, catalog: true, story: true },
}

const server = setupServer()

beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => server.resetHandlers())
afterAll(() => server.close())

/** Build an API client wired to the mock transport with a spy login hook. */
function makeClient(overrides?: {
  onUnauthorized?: () => void
  config?: Partial<ApiConfig>
}): { api: ApiClient; onUnauthorized: ReturnType<typeof vi.fn> } {
  const onUnauthorized = vi.fn(overrides?.onUnauthorized)
  const http = createHttpClient({
    baseUrl: BASE,
    onUnauthorized,
    // Stub backoff so retry tests don't actually wait.
    sleep: () => Promise.resolve(),
    retry: { maxRetries: 2, baseDelayMs: 1, maxDelayMs: 1 },
  })
  const api = createApiClient({ http, config: { ...testConfig, ...overrides?.config } })
  return { api, onUnauthorized }
}

describe('leaderboard.list', () => {
  it('decodes a typed leaderboard page and forwards query params', async () => {
    const seen: URLSearchParams[] = []
    server.use(
      http.get(`${BASE}/leaderboard`, ({ request }) => {
        seen.push(new URL(request.url).searchParams)
        return HttpResponse.json({
          seasonId: 'S-2026-07',
          entries: [{ rank: 1, playerId: 'p1', displayName: 'Ada', rating: 2400, stars: 5 }],
          total: 1,
          page: 0,
          pageSize: 20,
        })
      }),
    )

    const { api } = makeClient()
    const page = await api.leaderboard.list({ seasonId: 'S-2026-07', page: 0, pageSize: 20 })

    expect(page.entries[0].displayName).toBe('Ada')
    expect(page.total).toBe(1)
    expect(seen[0].get('seasonId')).toBe('S-2026-07')
    expect(seen[0].get('pageSize')).toBe('20')
  })
})

describe('shop.createOrder', () => {
  it('POSTs the JSON body and returns the created order', async () => {
    let received: unknown
    server.use(
      http.post(`${BASE}/shop/orders`, async ({ request }) => {
        received = await request.json()
        return HttpResponse.json(
          {
            orderId: 'o-1',
            playerId: 'p1',
            lineItems: ['pack.core'],
            currency: 'USD',
            status: 'created',
          },
          { status: 201 },
        )
      }),
    )

    const { api } = makeClient()
    const order = await api.shop.createOrder({
      playerId: 'p1',
      lineItems: ['pack.core'],
      currency: 'USD',
    })

    expect(order.status).toBe('created')
    expect(received).toEqual({ playerId: 'p1', lineItems: ['pack.core'], currency: 'USD' })
  })
})

describe('story.listMissions', () => {
  it('decodes the campaign for a player', async () => {
    server.use(
      http.get(`${BASE}/story/:playerId/missions`, ({ params }) =>
        HttpResponse.json({
          playerId: params.playerId,
          missions: [
            {
              missionId: 'm-1',
              name: 'The Awakening',
              description: 'Face the first boss.',
              difficultyTier: 'Prologue',
              boss: {
                bossId: 'b-1',
                name: 'Gravemind',
                startingHp: 30,
                heroPower: 'Reap',
                trademark: 'Undying',
                signatureCardIds: ['c-9'],
              },
              aiProfile: {
                profileId: 'ai-1',
                difficultyTier: 'Prologue',
                strategyKind: 'Scripted',
                mctsBudget: 0,
              },
              firstClearRewardClaimed: false,
              unlocked: true,
            },
          ],
        }),
      ),
    )

    const { api } = makeClient()
    const story = await api.story.listMissions('p1')
    expect(story.playerId).toBe('p1')
    expect(story.missions[0].boss.name).toBe('Gravemind')
    expect(story.missions[0].aiProfile.strategyKind).toBe('Scripted')
  })
})

describe('story.launchAttempt', () => {
  it('POSTs the launch body and returns the attempt with its match ticket', async () => {
    let received: unknown
    server.use(
      http.post(`${BASE}/story/:playerId/missions/:missionId/attempts`, async ({ request }) => {
        received = await request.json()
        return HttpResponse.json(
          {
            attemptId: 'a-1',
            missionId: 'm-1',
            playerId: 'p1',
            difficultyTier: 'Prologue',
            matchTicket: 't-mission-1',
            scriptedStateStep: 0,
            missionCompleted: false,
          },
          { status: 201 },
        )
      }),
    )

    const { api } = makeClient()
    const attempt = await api.story.launchAttempt('p1', 'm-1', { playerId: 'p1', missionId: 'm-1' })
    expect(attempt.matchTicket).toBe('t-mission-1')
    expect(received).toEqual({ playerId: 'p1', missionId: 'm-1' })
  })
})

describe('401 handling', () => {
  it('invokes the login-redirect hook and throws without retrying', async () => {
    let calls = 0
    server.use(
      http.get(`${BASE}/collection/:playerId`, () => {
        calls += 1
        return new HttpResponse(null, { status: 401 })
      }),
    )

    const { api, onUnauthorized } = makeClient()
    await expect(api.collection.get('p1')).rejects.toMatchObject({
      name: 'ApiError',
      status: 401,
    })
    expect(onUnauthorized).toHaveBeenCalledTimes(1)
    expect(calls).toBe(1) // a 401 is terminal — no retry
  })
})

describe('error normalization', () => {
  it('reads the backend error envelope into ApiError fields', async () => {
    server.use(
      http.get(`${BASE}/catalog/cards/:cardId`, () =>
        HttpResponse.json(
          { code: 'card_not_found', message: 'no such card', details: { cardId: 'x' } },
          { status: 404 },
        ),
      ),
    )

    const { api } = makeClient()
    const err = await api.catalog.getCard('x').catch((e: unknown) => e)
    expect(err).toBeInstanceOf(ApiError)
    const apiErr = err as ApiError
    expect(apiErr.kind).toBe('http')
    expect(apiErr.status).toBe(404)
    expect(apiErr.code).toBe('card_not_found')
    expect(apiErr.message).toBe('no such card')
    expect(apiErr.retriable).toBe(false)
  })

  it('normalizes a transport failure to kind "network"', async () => {
    server.use(http.get(`${BASE}/shop/items`, () => HttpResponse.error()))

    const { api } = makeClient()
    const err = (await api.shop.listItems().catch((e: unknown) => e)) as ApiError
    expect(err).toBeInstanceOf(ApiError)
    expect(err.kind).toBe('network')
  })
})

describe('retry/backoff', () => {
  it('retries a 503 then succeeds', async () => {
    let attempts = 0
    server.use(
      http.get(`${BASE}/catalog/cards`, () => {
        attempts += 1
        if (attempts < 3) return new HttpResponse(null, { status: 503 })
        return HttpResponse.json([
          {
            cardId: 'c1',
            name: 'Spark',
            cost: 1,
            cardClass: 'aggression',
            cardType: 'spell',
            rarity: 'common',
            keywords: [],
            effectScriptRef: 'fx.spark',
            copyCap: 3,
          },
        ])
      }),
    )

    const { api } = makeClient()
    const cards = await api.catalog.listCards()
    expect(attempts).toBe(3) // 1 initial + 2 retries
    expect(cards[0].name).toBe('Spark')
  })

  it('gives up after exhausting retries on a persistent 500', async () => {
    let attempts = 0
    server.use(
      http.get(`${BASE}/catalog/cards`, () => {
        attempts += 1
        return new HttpResponse(null, { status: 500 })
      }),
    )

    const { api } = makeClient()
    await expect(api.catalog.listCards()).rejects.toMatchObject({ status: 500 })
    expect(attempts).toBe(3) // 1 initial + 2 retries, then throw
  })
})

describe('capability gating', () => {
  it('refuses a disabled resource before touching the network', async () => {
    const { api } = makeClient({ config: { capabilities: { ...testConfig.capabilities, shop: false } } })
    const err = (await api.shop.listItems().catch((e: unknown) => e)) as ApiError
    expect(err).toBeInstanceOf(ApiError)
    expect(err.kind).toBe('disabled')
  })
})

describe('realtime.gameSocketUrl', () => {
  it('builds the ws handshake URL with the ticket query', () => {
    const { api } = makeClient()
    expect(api.realtime.gameSocketUrl()).toBe('wss://ws.testnet.made.vforce360.ai/ws')
    expect(api.realtime.gameSocketUrl({ ticket: 't-42' })).toBe(
      'wss://ws.testnet.made.vforce360.ai/ws?ticket=t-42',
    )
  })
})
