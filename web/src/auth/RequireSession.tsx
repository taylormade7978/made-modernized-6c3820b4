import { Navigate, Outlet } from 'react-router-dom'
import { LOGIN_ROUTE } from './session'
import { useSession } from './SessionProvider'

/**
 * Route guard for the authenticated app shell. Unauthenticated, expired, or
 * unreachable-endpoint visitors are routed to the login entry view (where the
 * gateway hand-off happens); authenticated visitors fall through to the routed
 * <Outlet>.
 */
export default function RequireSession() {
  const { state } = useSession()

  if (state.status === 'loading') {
    return (
      <div className="gate" role="status" aria-live="polite">
        <p className="gate__msg">Checking your session…</p>
      </div>
    )
  }

  if (state.status === 'ready' && state.session.authenticated) {
    return <Outlet />
  }

  return <Navigate to={LOGIN_ROUTE} replace />
}
