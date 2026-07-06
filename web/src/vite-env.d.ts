/// <reference types="vite/client" />
/// <reference types="vite-plugin-pwa/client" />

// Build-time capability constants injected by `define` in vite.config.ts.
// They are replaced with literal `true`/`false` (or a string) at build time,
// so guards written against them are dead-code-eliminated.
declare const __CAP_TOKEN__: boolean
declare const __CAP_MARKETPLACE__: boolean
declare const __CAP_WALLET__: boolean
declare const __CAP_REDIRECT_BASE_URL__: string

// Runtime env for the edge/session config (see src/config/session.ts). Unlike
// the capability flags, these are ordinary runtime values read from
// import.meta.env — no dead-code elimination depends on them. Interface merging
// augments Vite's built-in ImportMetaEnv with our typed keys.
interface ImportMetaEnv {
  readonly VITE_SESSION_ENDPOINT?: string
  readonly VITE_GATEWAY_SIGNIN_URL?: string
  readonly VITE_GATEWAY_SIGNOUT_URL?: string
  readonly VITE_GATEWAY_RETURN_PARAM?: string

  // API / realtime endpoint config (see src/config/api.ts).
  readonly VITE_API_ENV?: string
  readonly VITE_API_PROJECT?: string
  readonly VITE_API_BASE_URL?: string
  readonly VITE_WS_BASE_URL?: string
  readonly VITE_API_CAP_COLLECTION?: string
  readonly VITE_API_CAP_LEADERBOARD?: string
  readonly VITE_API_CAP_SHOP?: string
  readonly VITE_API_CAP_CATALOG?: string
  readonly VITE_API_CAP_STORY?: string
}
