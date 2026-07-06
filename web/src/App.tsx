import { RouterProvider, createHashRouter } from 'react-router-dom'
import { routes } from './routes'
import { SessionProvider } from './auth/SessionProvider'

// Hash routing keeps the bundle shell-agnostic: it needs no server-side URL
// rewrite and resolves correctly over the `file://` / `capacitor://` origins a
// native container serves from, so the same `dist/` runs on the web and inside
// Capacitor without code changes.
const router = createHashRouter(routes)

// SessionProvider sits ABOVE the router so a single edge-session check backs
// both the route guard and the login view; the router's elements read it
// through React context.
export default function App() {
  return (
    <SessionProvider>
      <RouterProvider router={router} />
    </SessionProvider>
  )
}
