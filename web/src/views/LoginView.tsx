import { Link } from 'react-router-dom'
import { redirectToSignIn, signOutUrl } from '../auth/session'
import { useSession } from '../auth/SessionProvider'

/**
 * Login / identity entry view.
 *
 * Surfaces the *gateway-driven* sign-in flow: it holds no password fields, no
 * JWT parsing, and no OAuth client logic (the Kong/OPA edge owns all of that).
 * It reads the edge-asserted session and either shows the signed-in
 * identity/tenant with a way into the app, or directs an unauthenticated
 * visitor to the gateway sign-in entry.
 */
export default function LoginView() {
  const { state, refresh } = useSession()

  if (state.status === 'loading') {
    return (
      <section className="login" role="status" aria-live="polite">
        <p className="login__status">Checking your session…</p>
      </section>
    )
  }

  if (state.status === 'ready' && state.session.authenticated) {
    const { identity } = state.session
    const out = signOutUrl()
    return (
      <section className="login" aria-labelledby="login-title">
        <h1 id="login-title" className="login__title">You&rsquo;re signed in</h1>
        <dl className="login__identity">
          <dt>Identity</dt>
          <dd>{identity.displayName || identity.subject || 'Unknown'}</dd>
          {identity.tenant ? (
            <>
              <dt>Tenant</dt>
              <dd>{identity.tenant}</dd>
            </>
          ) : null}
        </dl>
        <Link className="login__cta" to="/match">
          Continue to MADE
        </Link>
        {out ? (
          <a className="login__link" href={out}>
            Sign out
          </a>
        ) : null}
      </section>
    )
  }

  // Anonymous, expired session, or a failed session check.
  const failed = state.status === 'error'
  return (
    <section className="login" aria-labelledby="login-title">
      <h1 id="login-title" className="login__title">MADE</h1>
      <p className="login__blurb">
        {failed
          ? 'We could not reach the sign-in service. Try again in a moment.'
          : 'Sign in through the secure gateway to play.'}
      </p>
      <button className="login__cta" type="button" onClick={() => redirectToSignIn()}>
        Sign in
      </button>
      {failed ? (
        <button className="login__link" type="button" onClick={refresh}>
          Retry
        </button>
      ) : null}
      <p className="login__note">
        Authentication is handled by the gateway — MADE never sees your password.
      </p>
    </section>
  )
}
