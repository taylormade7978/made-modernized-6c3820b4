/**
 * Stage the compiled rules-WASM pkg into the bundle's static assets.
 *
 * `wasm-pack build crates/game-session --target web` emits `game_session.js` +
 * `game_session_bg.wasm` into `crates/game-session/pkg/`. We copy those into
 * `web/public/vendor/game-session/` so Vite treats them as static assets and
 * copies them verbatim into `dist/vendor/game-session/`, where:
 *   - index.html's import map resolves the `game-session` specifier to them, and
 *   - workbox's wasm precache glob picks them up for offline / OTA launches.
 *
 * Run automatically before every build via the `prebuild` npm hook. It is a
 * no-op (with a warning) when the pkg is absent, so a plain `npm run build`
 * without the Rust/wasm toolchain still succeeds — the loader degrades to
 * "gate disabled" at runtime.
 */
import { cpSync, existsSync, mkdirSync, rmSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const webRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const pkgDir = resolve(webRoot, '../crates/game-session/pkg')
const destDir = resolve(webRoot, 'public/vendor/game-session')

// The two runtime files (js loader + wasm binary). The .d.ts / package.json in
// pkg/ are not needed by the bundle, so we copy only what ships.
const artifacts = ['game_session.js', 'game_session_bg.wasm']

const missing = artifacts.filter((f) => !existsSync(resolve(pkgDir, f)))
if (missing.length > 0) {
  console.warn(
    `[stage-wasm] rules-WASM pkg not found (missing: ${missing.join(', ')}). ` +
      `Skipping — build the artifact with \`wasm-pack build crates/game-session ` +
      `--target web -- --features wasm\` to include the shared rules gate. ` +
      `The bundle stays valid; the WASM name-gate degrades to disabled at runtime.`,
  )
  process.exit(0)
}

rmSync(destDir, { recursive: true, force: true })
mkdirSync(destDir, { recursive: true })
for (const file of artifacts) {
  cpSync(resolve(pkgDir, file), resolve(destDir, file))
}
console.log(`[stage-wasm] staged ${artifacts.length} rules-WASM artifact(s) → public/vendor/game-session/`)
