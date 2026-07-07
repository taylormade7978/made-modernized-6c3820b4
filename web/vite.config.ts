import { defineConfig, loadEnv } from 'vite'
import react from '@vitejs/plugin-react'
import { VitePWA } from 'vite-plugin-pwa'

// Parse a `VITE_CAP_*` flag; unset/empty => default (enabled).
function flag(raw: string | undefined, fallback: boolean): boolean {
  if (raw === undefined || raw === '') return fallback
  return raw === 'true' || raw === '1'
}

// Shell-agnostic bundle: `base: './'` emits relative asset URLs so the exact
// same `dist/` loads over http(s) in a browser AND over the `capacitor://` /
// `file://` origins a Capacitor native container serves from — no code changes.
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), 'VITE_')

  // Compute capability booleans here (build/Node side) and inject them as real
  // literal globals via `define`. Because the guards in routes.tsx / AppLayout
  // read `__CAP_*__` directly, esbuild folds `if (false) import('./TokenView')`
  // to nothing and Rollup prunes the orphaned chunk — the token/marketplace/
  // wallet routes are ELIMINATED from a native-shell build, not just hidden.
  const cap = {
    token: flag(env.VITE_CAP_TOKEN, true),
    marketplace: flag(env.VITE_CAP_MARKETPLACE, true),
    wallet: flag(env.VITE_CAP_WALLET, true),
    redirectBaseUrl: env.VITE_CAP_REDIRECT_BASE_URL ?? '',
  }

  return {
    base: './',
    define: {
      __CAP_TOKEN__: JSON.stringify(cap.token),
      __CAP_MARKETPLACE__: JSON.stringify(cap.marketplace),
      __CAP_WALLET__: JSON.stringify(cap.wallet),
      __CAP_REDIRECT_BASE_URL__: JSON.stringify(cap.redirectBaseUrl),
    },
    plugins: [
      react(),
      VitePWA({
        // Register + auto-update the service worker without user prompts, so OTA
        // bundle refreshes land on next launch (the "offline/OTA bundles" AC).
        registerType: 'autoUpdate',
        injectRegister: 'auto',
        includeAssets: ['favicon.svg', 'icons/apple-touch-icon.png'],
        manifest: {
          name: 'MADE — Card Game',
          short_name: 'MADE',
          description: 'MADE modernized card game — mobile-first PWA client.',
          id: '/',
          start_url: './',
          scope: './',
          display: 'standalone',
          orientation: 'portrait',
          background_color: '#0b0d12',
          theme_color: '#0b0d12',
          icons: [
            { src: 'icons/pwa-192.png', sizes: '192x192', type: 'image/png' },
            { src: 'icons/pwa-512.png', sizes: '512x512', type: 'image/png' },
            {
              src: 'icons/pwa-maskable-512.png',
              sizes: '512x512',
              type: 'image/png',
              purpose: 'maskable',
            },
          ],
        },
        workbox: {
          // Precache the app shell so cold launches work offline (installability).
          globPatterns: ['**/*.{js,css,html,svg,png,ico,woff2,wasm}'],
          navigateFallback: 'index.html',
          // Auth + API paths must reach the network (the oauth2-proxy gateway
          // and the backend), never the SPA shell. Without this denylist the
          // service worker serves index.html for /oauth2/start, hijacking the
          // sign-in redirect and loading the app under /oauth2/ (assets then
          // resolve to /oauth2/assets/* and are blocked).
          navigateFallbackDenylist: [/^\/oauth2\//, /^\/api\//],
          cleanupOutdatedCaches: true,
        },
        devOptions: { enabled: false },
      }),
    ],
  }
})
