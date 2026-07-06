/**
 * Public entry point for the API client layer.
 *
 * App code should import from `../api` (or `../api/index`): the default
 * {@link api} singleton for calls, the typed DTOs for props/state, and
 * {@link ApiError} for error handling in views.
 */
export { api, createApiClient, defaultHttpConfig } from './client'
export type {
  ApiClient,
  CallOptions,
  CatalogApi,
  CollectionApi,
  CreateApiClientConfig,
  GameSocketParams,
  LeaderboardApi,
  RealtimeApi,
  ShopApi,
  StoryApi,
} from './client'

export { createHttpClient, buildUrl, backoffDelay } from './http'
export type {
  HttpClient,
  HttpClientConfig,
  QueryParams,
  QueryValue,
  RequestOptions,
  RetryConfig,
} from './http'

export { ApiError, defaultRetriable, errorFromResponse, errorFromThrown } from './errors'
export type { ApiErrorKind, ApiErrorBody, ApiErrorInit } from './errors'

export * from './types'
