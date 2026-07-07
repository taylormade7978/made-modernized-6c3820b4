/**
 * Minimal GraphQL query transport for reads.
 *
 * The app's read surface is GraphQL (searchable/filterable queries); mutations
 * stay REST (see client.ts) and live updates arrive via WebSocket subscriptions
 * (see realtime.ts). This helper is intentionally tiny: POST the query with the
 * edge session cookie, unwrap `data`, and normalize failures into an ApiError.
 */
import { ApiError } from './errors'

export interface GraphqlConfig {
  readonly url: string
  /** Invoked on a 401 before throwing — the login redirect (parity with REST). */
  readonly onUnauthorized?: () => void
}

export async function gql<T>(
  config: GraphqlConfig,
  query: string,
  variables?: Record<string, unknown>,
  signal?: AbortSignal,
): Promise<T> {
  let res: Response
  try {
    res = await fetch(config.url, {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ query, variables }),
      signal,
    })
  } catch (cause) {
    throw new ApiError({ kind: 'network', status: 0, message: 'graphql request failed', cause })
  }
  if (res.status === 401 || res.status === 403) {
    config.onUnauthorized?.()
    throw new ApiError({ kind: 'http', status: res.status, message: 'unauthorized' })
  }
  const json = (await res.json().catch(() => null)) as { data?: T; errors?: { message: string }[] } | null
  if (!json) throw new ApiError({ kind: 'parse', status: res.status, message: 'failed to parse graphql response' })
  if (json.errors?.length) {
    throw new ApiError({ kind: 'http', status: res.status, message: json.errors.map((e) => e.message).join('; ') })
  }
  return json.data as T
}
