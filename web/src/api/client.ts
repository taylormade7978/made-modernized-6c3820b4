/**
 * The typed API client: one method per consumed `/v1` endpoint, grouped by
 * resource, plus the realtime WebSocket handshake-URL builder.
 *
 * `createApiClient` is a factory over an {@link HttpClient} and the endpoint
 * config, so tests can wire a mocked transport and a spy login hook. A default
 * singleton {@link api} is exported for app code, built from `apiConfig` and the
 * S-79 `redirectToLogin` hook.
 */
import { apiConfig, type ApiConfig } from '../config/api'
import { redirectToLogin } from '../auth/session'
import { createHttpClient, type HttpClient, type HttpClientConfig } from './http'
import { gql } from './graphql'
import { ApiError } from './errors'
import type {
  CollectionResponse,
  CreateOrderRequest,
  Deck,
  Card,
  ExpansionSet,
  LaunchMissionRequest,
  LeaderboardPage,
  LeaderboardQuery,
  MissionAttempt,
  Order,
  SaveDeckRequest,
  ShopItem,
  StoryResponse,
} from './types'

/** Options accepted by every call: an abort signal for cancellation. */
export interface CallOptions {
  readonly signal?: AbortSignal
}

/** Parameters for the authoritative live-match WebSocket handshake. */
export interface GameSocketParams {
  /** Matchmaking ticket / match id the socket should join, when known. */
  readonly ticket?: string
}

export interface ApiClient {
  readonly collection: CollectionApi
  readonly leaderboard: LeaderboardApi
  readonly shop: ShopApi
  readonly catalog: CatalogApi
  readonly story: StoryApi
  readonly realtime: RealtimeApi
}

export interface CollectionApi {
  /** `GET /v1/collection/{playerId}` — owned cards + decks. */
  get(playerId: string, opts?: CallOptions): Promise<CollectionResponse>
  /** `PUT /v1/collection/{playerId}/decks/{deckId}` — create/update a deck. */
  saveDeck(
    playerId: string,
    deckId: string,
    body: SaveDeckRequest,
    opts?: CallOptions,
  ): Promise<Deck>
}

export interface LeaderboardApi {
  /** `GET /v1/leaderboard` — a page of ranked standings. */
  list(query?: LeaderboardQuery, opts?: CallOptions): Promise<LeaderboardPage>
}

export interface ShopApi {
  /** `GET /v1/shop/items` — purchasable items. */
  listItems(opts?: CallOptions): Promise<readonly ShopItem[]>
  /** `POST /v1/shop/orders` — start an order. */
  createOrder(body: CreateOrderRequest, opts?: CallOptions): Promise<Order>
  /** `GET /v1/shop/orders/{orderId}` — an order's current state. */
  getOrder(orderId: string, opts?: CallOptions): Promise<Order>
}

export interface StoryApi {
  /** `GET /v1/story/{playerId}/missions` — the player's campaign (missions + bosses). */
  listMissions(playerId: string, opts?: CallOptions): Promise<StoryResponse>
  /**
   * `POST /v1/story/{playerId}/missions/{missionId}/attempts` — launch a
   * MissionAttempt against the AI-opponent service; returns the attempt and the
   * match ticket that joins the authoritative match to play it.
   */
  launchAttempt(
    playerId: string,
    missionId: string,
    body: LaunchMissionRequest,
    opts?: CallOptions,
  ): Promise<MissionAttempt>
}

export interface CatalogApi {
  /** `GET /v1/catalog/cards` — all published card definitions. */
  listCards(opts?: CallOptions): Promise<readonly Card[]>
  /** `GET /v1/catalog/cards/{cardId}` — a single card definition. */
  getCard(cardId: string, opts?: CallOptions): Promise<Card>
  /** `GET /v1/catalog/expansions` — released expansion sets. */
  listExpansions(opts?: CallOptions): Promise<readonly ExpansionSet[]>
}

export interface RealtimeApi {
  /**
   * Build the WebSocket handshake URL for the authoritative game connection.
   * The socket carries the edge session cookie on the upgrade request, so no
   * token is placed in the URL; only the optional matchmaking `ticket` is.
   */
  gameSocketUrl(params?: GameSocketParams): string
}

export interface CreateApiClientConfig {
  readonly http: HttpClient
  /** Endpoint config (base URLs, capability flags). Defaults to {@link apiConfig}. */
  readonly config?: ApiConfig
}

/**
 * Guard a resource behind its environment capability flag. Returns a *rejected
 * promise* (never a synchronous throw) when disabled, so every client method —
 * success or failure — is uniformly a promise a view can `.catch()`.
 */
function guard<T>(enabled: boolean, resource: string, env: string, run: () => Promise<T>): Promise<T> {
  if (!enabled) {
    return Promise.reject(
      new ApiError({
        kind: 'disabled',
        message: `the "${resource}" API is disabled in the ${env} environment`,
      }),
    )
  }
  return run()
}

/** Percent-encode a single path segment (ids can contain reserved chars). */
function seg(value: string): string {
  return encodeURIComponent(value)
}

export function createApiClient({ http, config = apiConfig }: CreateApiClientConfig): ApiClient {
  const cap = config.capabilities
  const env = config.env

  // Reads go through GraphQL (queries, searchable); mutations stay REST (below).
  const gcfg = { url: config.graphqlUrl, onUnauthorized: redirectToLogin }
  const CARD_FIELDS = 'cardId name cost cardClass cardType rarity keywords effectScriptRef copyCap art text heat atk hp artTint'

  const collection: CollectionApi = {
    get(playerId, opts) {
      return guard(cap.collection, 'collection', env, () =>
        gql<{ collection: CollectionResponse }>(
          gcfg,
          `query($p:ID!){ collection(playerId:$p){ playerId ownedCards{ cardId quantity cosmeticSkinRef } decks{ deckId name cardIds active } } }`,
          { p: playerId },
          opts?.signal,
        ).then((d) => d.collection),
      )
    },
    saveDeck(playerId, deckId, body, opts) {
      return guard(cap.collection, 'collection', env, () =>
        http.request<Deck>(`/collection/${seg(playerId)}/decks/${seg(deckId)}`, {
          method: 'PUT',
          body,
          signal: opts?.signal,
        }),
      )
    },
  }

  const leaderboard: LeaderboardApi = {
    list(query, opts) {
      return guard(cap.leaderboard, 'leaderboard', env, () =>
        gql<{ leaderboard: LeaderboardPage }>(
          gcfg,
          `query($page:Int,$size:Int){ leaderboard(page:$page,pageSize:$size){ seasonId total page pageSize entries{ rank playerId displayName rating stars } } }`,
          { page: query?.page, size: query?.pageSize },
          opts?.signal,
        ).then((d) => d.leaderboard),
      )
    },
  }

  const shop: ShopApi = {
    listItems(opts) {
      return guard(cap.shop, 'shop', env, () =>
        gql<{ shopItems: readonly ShopItem[] }>(
          gcfg,
          `{ shopItems{ sku name description kind settlement priceMinor currency } }`,
          undefined,
          opts?.signal,
        ).then((d) => d.shopItems),
      )
    },
    createOrder(body, opts) {
      return guard(cap.shop, 'shop', env, () =>
        http.request<Order>('/shop/orders', { method: 'POST', body, signal: opts?.signal }),
      )
    },
    getOrder(orderId, opts) {
      return guard(cap.shop, 'shop', env, () =>
        http.request<Order>(`/shop/orders/${seg(orderId)}`, { signal: opts?.signal }),
      )
    },
  }

  const catalog: CatalogApi = {
    listCards(opts) {
      return guard(cap.catalog, 'catalog', env, () =>
        gql<{ cards: readonly Card[] }>(gcfg, `{ cards{ ${CARD_FIELDS} } }`, undefined, opts?.signal).then(
          (d) => d.cards,
        ),
      )
    },
    getCard(cardId, opts) {
      return guard(cap.catalog, 'catalog', env, () =>
        gql<{ card: Card }>(gcfg, `query($id:ID!){ card(cardId:$id){ ${CARD_FIELDS} } }`, { id: cardId }, opts?.signal).then(
          (d) => d.card,
        ),
      )
    },
    listExpansions(opts) {
      return guard(cap.catalog, 'catalog', env, () =>
        http.request<readonly ExpansionSet[]>('/catalog/expansions', { signal: opts?.signal }),
      )
    },
  }

  const story: StoryApi = {
    listMissions(playerId, opts) {
      return guard(cap.story, 'story', env, () =>
        http.request<StoryResponse>(`/story/${seg(playerId)}/missions`, { signal: opts?.signal }),
      )
    },
    launchAttempt(playerId, missionId, body, opts) {
      return guard(cap.story, 'story', env, () =>
        http.request<MissionAttempt>(
          `/story/${seg(playerId)}/missions/${seg(missionId)}/attempts`,
          { method: 'POST', body, signal: opts?.signal },
        ),
      )
    },
  }

  const realtime: RealtimeApi = {
    gameSocketUrl(params) {
      const url = new URL('/ws', `${config.wsBaseUrl}/`)
      if (params?.ticket) url.searchParams.set('ticket', params.ticket)
      return url.toString()
    },
  }

  return { collection, leaderboard, shop, catalog, story, realtime }
}

/**
 * Default HTTP client config for the app: points at the resolved REST base and
 * wires the 401 → login redirect hook from S-79.
 */
export const defaultHttpConfig: HttpClientConfig = {
  baseUrl: apiConfig.restBaseUrl,
  onUnauthorized: redirectToLogin,
}

/** App-wide singleton API client. */
export const api: ApiClient = createApiClient({ http: createHttpClient(defaultHttpConfig) })
