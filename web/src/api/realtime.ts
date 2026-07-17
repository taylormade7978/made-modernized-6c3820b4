/**
 * Realtime subscription client (graphql-ws over WebSocket).
 *
 * The app's live-update surface: components subscribe to a GraphQL subscription
 * and receive server-pushed frames on change — no polling. The socket carries
 * the edge session cookie on the upgrade handshake (same-origin credentials),
 * so no token is placed in the URL.
 */
import { createClient, type Client } from 'graphql-ws'

import { apiConfig } from '../config/api'

let client: Client | null = null

function getClient(): Client {
  if (!client) {
    client = createClient({ url: `${apiConfig.wsBaseUrl}/graphql`, lazy: true, retryAttempts: Infinity })
  }
  return client
}

/**
 * Subscribe to a GraphQL subscription. `onNext` fires for every pushed frame;
 * returns an unsubscribe function. Errors are swallowed to a console warning so
 * a dropped socket never crashes a view (it reconnects).
 */
export function subscribe<T>(
  query: string,
  variables: Record<string, unknown>,
  onNext: (data: T) => void,
): () => void {
  const dispose = getClient().subscribe<T>(
    { query, variables },
    {
      next: (msg) => {
        if (msg.data) onNext(msg.data)
      },
      error: (err) => console.warn('realtime subscription error', err),
      complete: () => {},
    },
  )
  return dispose
}
