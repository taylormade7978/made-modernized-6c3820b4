import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'
import { fetchSession, type Session } from './session'

/** Session as observed by the UI, including the in-flight "loading" phase. */
export type SessionState =
  | { readonly status: 'loading' }
  | { readonly status: 'ready'; readonly session: Session }
  | { readonly status: 'error'; readonly error: string }

interface SessionContextValue {
  readonly state: SessionState
  /** Re-query the session endpoint (e.g. after returning from the gateway). */
  refresh: () => void
}

const SessionContext = createContext<SessionContextValue | null>(null)

/**
 * Fetches the edge-asserted session once on mount and shares it with the whole
 * tree. Sitting above <RouterProvider>, a single check backs both the route
 * guard and the login view, so returning from the gateway resolves identity
 * without a second round-trip.
 */
export function SessionProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<SessionState>({ status: 'loading' })
  const [nonce, setNonce] = useState(0)

  useEffect(() => {
    const ctrl = new AbortController()
    setState({ status: 'loading' })
    fetchSession(ctrl.signal)
      .then((session) => setState({ status: 'ready', session }))
      .catch((err: unknown) => {
        if (ctrl.signal.aborted) return
        setState({
          status: 'error',
          error: err instanceof Error ? err.message : 'session check failed',
        })
      })
    return () => ctrl.abort()
  }, [nonce])

  return (
    <SessionContext.Provider value={{ state, refresh: () => setNonce((n) => n + 1) }}>
      {children}
    </SessionContext.Provider>
  )
}

export function useSession(): SessionContextValue {
  const ctx = useContext(SessionContext)
  if (!ctx) throw new Error('useSession must be used within <SessionProvider>')
  return ctx
}
