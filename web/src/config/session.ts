/**
 * Runtime edge / session configuration.
 *
 * The PWA never implements authentication itself: a Kong/OPA edge terminates
 * OIDC, validates the token, and injects *trusted identity headers* on every
 * upstream request. The app only needs two URLs:
 *
 *  - `sessionEndpoint` — a backend "who am I" endpoint that echoes the identity
 *    the edge asserted (from those trusted headers) as JSON. The PWA reads it to
 *    learn the signed-in identity/tenant; it does NOT parse a JWT, a cookie, or
 *    a password.
 *  - `signInUrl` — the gateway-driven sign-in entry. Redirecting the browser
 *    here hands off to the edge's OIDC flow; the edge redirects back once a
 *    session is established.
 *
 * These are read from `VITE_*` env at build time (see `.env.example`) with
 * sensible same-origin defaults, so a stock build works behind the edge with no
 * extra config. Unlike the `__CAP_*__` capability flags (which are `define`
 * literals so branches dead-code-eliminate), these are ordinary runtime config
 * — no build-time branching depends on them — so `import.meta.env` is the right
 * mechanism.
 */
export interface SessionConfig {
  /** Backend endpoint that echoes edge-asserted identity/tenant as JSON. */
  readonly sessionEndpoint: string
  /** Gateway sign-in entry URL (redirect target for the OIDC hand-off). */
  readonly signInUrl: string
  /** Gateway sign-out URL (clears the edge session), or "" to hide sign-out. */
  readonly signOutUrl: string
  /** Query param the gateway reads for the post-sign-in return URL. */
  readonly returnParam: string
}

export const sessionConfig: SessionConfig = {
  sessionEndpoint: import.meta.env.VITE_SESSION_ENDPOINT ?? '/api/session',
  signInUrl: import.meta.env.VITE_GATEWAY_SIGNIN_URL ?? '/oauth2/start',
  signOutUrl: import.meta.env.VITE_GATEWAY_SIGNOUT_URL ?? '/oauth2/sign_out',
  returnParam: import.meta.env.VITE_GATEWAY_RETURN_PARAM ?? 'rd',
}
