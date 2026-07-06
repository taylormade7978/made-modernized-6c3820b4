import { sessionConfig } from '../config/session'

/** Identity the edge asserted for the current session. */
export interface Identity {
  /** Stable subject id from the edge (opaque; NOT decoded client-side). */
  readonly subject: string
  /** Human-readable display name, when the edge provides one. */
  readonly displayName: string
  /** Tenant / organization the identity belongs to. */
  readonly tenant: string
}

/** Resolved session: either an authenticated identity or the anonymous state. */
export type Session =
  | { readonly authenticated: true; readonly identity: Identity }
  | { readonly authenticated: false }

/** JSON shape returned by the backend session ("who am I") endpoint. */
interface SessionResponse {
  readonly subject?: string
  readonly displayName?: string
  readonly tenant?: string
}

/** Hash route of the login / identity entry view. */
export const LOGIN_ROUTE = '/login'

/**
 * Ask the backend who the edge says we are.
 *
 * The response is populated by the trusted headers the Kong/OPA edge injects —
 * this call performs NO token parsing, password handling, or OAuth client logic
 * (all of that lives at the edge). A 401/403 means the edge did not assert an
 * identity (no / expired session): we resolve to the anonymous state rather than
 * throwing, so callers can route to the login entry view.
 */
export async function fetchSession(signal?: AbortSignal): Promise<Session> {
  const res = await fetch(sessionConfig.sessionEndpoint, {
    credentials: 'include',
    headers: { Accept: 'application/json' },
    signal,
  })
  if (res.status === 401 || res.status === 403) {
    return { authenticated: false }
  }
  if (!res.ok) {
    throw new Error(`session endpoint returned ${res.status}`)
  }
  const body = (await res.json()) as SessionResponse
  return {
    authenticated: true,
    identity: {
      subject: body.subject ?? '',
      displayName: body.displayName ?? body.subject ?? 'Signed-in user',
      tenant: body.tenant ?? '',
    },
  }
}

/**
 * Build the gateway sign-in URL, carrying a return target so the edge can send
 * the browser back where it started once the OIDC hand-off completes. Defaults
 * to the current location.
 */
export function buildSignInUrl(returnTo: string = window.location.href): string {
  const url = new URL(sessionConfig.signInUrl, window.location.origin)
  url.searchParams.set(sessionConfig.returnParam, returnTo)
  return url.toString()
}

/** Hand off to the gateway sign-in flow (full-page navigation). */
export function redirectToSignIn(returnTo?: string): void {
  window.location.assign(buildSignInUrl(returnTo))
}

/** Gateway sign-out URL, or `null` when sign-out is not configured. */
export function signOutUrl(): string | null {
  if (!sessionConfig.signOutUrl) return null
  return new URL(sessionConfig.signOutUrl, window.location.origin).toString()
}

/**
 * Route the browser to the login entry view (hash route). Used both as a plain
 * in-app navigation and as the session-expiry landing spot for {@link apiFetch}.
 */
export function redirectToLogin(): void {
  const target = `#${LOGIN_ROUTE}`
  if (window.location.hash !== target) {
    window.location.hash = target
  }
}

/**
 * `fetch` wrapper for authenticated API calls. When the edge rejects a call with
 * 401 (a session that expired mid-use), it routes the browser back to the login
 * entry view so the user can re-establish a session — giving every later
 * API-backed view the "session-expired 401 → login" behaviour for free.
 */
export async function apiFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const res = await fetch(input, { credentials: 'include', ...init })
  if (res.status === 401) {
    redirectToLogin()
  }
  return res
}
