/**
 * Adapter over the compiled `game-session` rules crate (`wasm-pack build
 * crates/game-session -- --features wasm`).
 *
 * The crate exposes `execute_command(sessionId, commandName)` — it runs a
 * command against a fresh `GameSession` and returns `Ok(())` when the *shared*
 * rules recognize and accept it, or the domain error text (e.g. an
 * `UnknownCommand`) otherwise. We use it as an authoritative **name-gate**: a
 * command the shared crate does not recognize can never be legal, so the
 * optimistic layer can refuse it before the local TS mirror even runs — this is
 * the point where the browser and server provably share one rules binary.
 *
 * Loading is best-effort and lazy. The `pkg/` output is a build artifact that is
 * absent from a stock `npm run build` (it needs the Rust/wasm toolchain), so the
 * import is dynamic and guarded: when the module cannot be loaded the gate
 * degrades to "allow" and the TS mirror in {@link module:rules} remains the
 * client-side authority. Nothing here is on the critical path for a first paint.
 */
import type { CommandName } from './model'

/** A loaded rules-WASM instance exposing the name-gate. */
export interface RulesWasm {
  /** True when the shared rules crate recognizes `command` as a real command. */
  recognizes(command: CommandName): boolean
}

/** The wasm-bindgen surface we depend on (a subset of the generated module). */
interface GameSessionModule {
  default?: (input?: unknown) => Promise<unknown>
  execute_command: (sessionId: string, commandName: string) => void
}

/**
 * Import specifier of the wasm-pack output. Kept in a variable and imported with
 * `@vite-ignore` so Vite does not try to resolve (and fail on) the artifact at
 * build time; it is only ever loaded at runtime, if it exists.
 */
// Annotated `string` (not the literal) so `tsc` treats the dynamic import as a
// runtime specifier and does not try to resolve the absent artifact at compile
// time; the build stays green without the wasm toolchain.
const PKG_SPECIFIER: string = 'game-session'

let cached: Promise<RulesWasm | null> | null = null

/**
 * Load the rules WASM once, memoized. Resolves to a {@link RulesWasm}, or `null`
 * when the artifact is unavailable (dev without a wasm build, or a plain web
 * bundle) — callers treat `null` as "gate disabled, trust the TS mirror".
 */
export function loadRulesWasm(): Promise<RulesWasm | null> {
  if (cached) return cached
  cached = importAndInit().catch(() => null)
  return cached
}

async function importAndInit(): Promise<RulesWasm | null> {
  const mod = (await import(/* @vite-ignore */ PKG_SPECIFIER)) as GameSessionModule
  // wasm-pack's web target exports a default init() that fetches the .wasm.
  if (typeof mod.default === 'function') {
    await mod.default()
  }
  if (typeof mod.execute_command !== 'function') return null

  return {
    recognizes(command) {
      try {
        // A recognized command that fails validation still *decoded* (it is not
        // an UnknownCommand); only an unrecognized name throws UnknownCommand.
        mod.execute_command('probe', command)
        return true
      } catch (err) {
        return !isUnknownCommand(err)
      }
    },
  }
}

/** True when a thrown JsValue is the aggregate's `UnknownCommand` decision. */
function isUnknownCommand(err: unknown): boolean {
  const text = typeof err === 'string' ? err : String((err as { message?: string })?.message ?? err)
  return /unknown command/i.test(text)
}
