/**
 * Runtime API / realtime endpoint configuration.
 *
 * The PWA talks to two backend surfaces:
 *
 *  - a REST API rooted at `https://api.{project}.vforce360.ai/v1` for the
 *    collection/deck, leaderboard, shop, and catalog resources, and
 *  - a WebSocket endpoint at `wss://ws.{project}.vforce360.ai` for the
 *    authoritative live-match connection.
 *
 * Both hosts follow the platform's `{service}.{project}.vforce360.ai` naming.
 * The base URLs are *environment-configurable* (dev / testnet / prod): the
 * environment selects a subdomain convention, and any piece can be overridden
 * outright via `VITE_*` env for local proxying or bespoke deployments.
 *
 * Like `config/session.ts` (and unlike the `__CAP_*__` capability literals),
 * these are ordinary runtime values read from `import.meta.env` — no build-time
 * dead-code elimination depends on them — so `import.meta.env` is the right
 * mechanism, with same-origin dev defaults so a stock `npm run dev` works behind
 * a local edge proxy with zero extra config.
 */

/** Deployment environment the client is pointed at. */
export type ApiEnv = 'dev' | 'testnet' | 'prod'

/** REST API version prefix. All resource paths hang off this. */
export const API_VERSION = 'v1'

export interface ApiConfig {
  /** Which deployment tier the base URLs resolve against. */
  readonly env: ApiEnv
  /** Project slug woven into the `{service}.{project}.vforce360.ai` hostnames. */
  readonly project: string
  /** REST base URL, including the `/v1` version prefix, without a trailing slash. */
  readonly restBaseUrl: string
  /** WebSocket base URL (scheme + host), without a trailing slash. */
  readonly wsBaseUrl: string
  /**
   * Per-environment capability flags. Complements the build-time `__CAP_*__`
   * flags (which physically strip native-shell-forbidden views): these gate
   * whether the client will *reach for* an endpoint on a given tier, so a
   * not-yet-deployed surface can be turned off per environment without a
   * rebuild. Unset flags default to enabled.
   */
  readonly capabilities: ApiCapabilities
}

/** Environment-configurable switches for the REST surfaces the client consumes. */
export interface ApiCapabilities {
  readonly collection: boolean
  readonly leaderboard: boolean
  readonly shop: boolean
  readonly catalog: boolean
  readonly story: boolean
}

/** Coerce a `VITE_*` env var into an {@link ApiEnv}, defaulting to `dev`. */
function readEnv(raw: string | undefined): ApiEnv {
  return raw === 'prod' || raw === 'testnet' ? raw : 'dev'
}

/** Parse a boolean-ish env var; unset/empty => `fallback`. */
function readFlag(raw: string | undefined, fallback: boolean): boolean {
  if (raw === undefined || raw === '') return fallback
  return raw === 'true' || raw === '1'
}

/**
 * Derive the default `{service}.{project}.vforce360.ai` host for an environment.
 * `prod` uses the bare `{service}.{project}` host; lower tiers interpose the
 * environment as a subdomain (`{service}.{env}.{project}`) so testnet and dev
 * are addressable without colliding with prod.
 */
function defaultHost(service: 'api' | 'ws', env: ApiEnv, project: string): string {
  const infix = env === 'prod' ? '' : `${env}.`
  return `${service}.${infix}${project}.vforce360.ai`
}

/**
 * Resolve the REST base URL. Precedence:
 *  1. explicit `VITE_API_BASE_URL` override (used verbatim, trailing slash trimmed),
 *  2. same-origin `/v1` in the `dev` tier (works behind a local edge proxy),
 *  3. the derived `https://api.{…}/v1` host otherwise.
 */
function resolveRestBaseUrl(env: ApiEnv, project: string): string {
  const override = import.meta.env.VITE_API_BASE_URL
  if (override) return trimSlash(override)
  if (env === 'dev') return `/${API_VERSION}`
  return `https://${defaultHost('api', env, project)}/${API_VERSION}`
}

/**
 * Resolve the WebSocket base URL. Precedence mirrors {@link resolveRestBaseUrl};
 * the `dev` default derives a same-origin `ws(s)://` URL from the page origin so
 * a local proxy can forward the socket.
 */
function resolveWsBaseUrl(env: ApiEnv, project: string): string {
  const override = import.meta.env.VITE_WS_BASE_URL
  if (override) return trimSlash(override)
  if (env === 'dev') return sameOriginWsBase()
  return `wss://${defaultHost('ws', env, project)}`
}

/** Map the current page origin to a same-origin `ws://`/`wss://` base. */
function sameOriginWsBase(): string {
  // `window` is absent under SSR / test harnesses; fall back to loopback so the
  // config object is always constructible.
  if (typeof window === 'undefined') return 'ws://localhost'
  const { protocol, host } = window.location
  const wsProtocol = protocol === 'https:' ? 'wss:' : 'ws:'
  return `${wsProtocol}//${host}`
}

/** Drop a single trailing slash so callers can join paths uniformly. */
function trimSlash(url: string): string {
  return url.endsWith('/') ? url.slice(0, -1) : url
}

const env = readEnv(import.meta.env.VITE_API_ENV)
const project = import.meta.env.VITE_API_PROJECT ?? 'made'

export const apiConfig: ApiConfig = {
  env,
  project,
  restBaseUrl: resolveRestBaseUrl(env, project),
  wsBaseUrl: resolveWsBaseUrl(env, project),
  capabilities: {
    collection: readFlag(import.meta.env.VITE_API_CAP_COLLECTION, true),
    leaderboard: readFlag(import.meta.env.VITE_API_CAP_LEADERBOARD, true),
    shop: readFlag(import.meta.env.VITE_API_CAP_SHOP, true),
    catalog: readFlag(import.meta.env.VITE_API_CAP_CATALOG, true),
    story: readFlag(import.meta.env.VITE_API_CAP_STORY, true),
  },
}
