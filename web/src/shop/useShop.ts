/**
 * `useShop` — the React controller behind the shop view.
 *
 * It loads the storefront catalog from the shop-payments-service and drives the
 * one purchase path this client is allowed to initiate: a **Stripe fiat
 * checkout**. Per the `order` aggregate's "fiat via Stripe only" invariant, the
 * controller never routes a `$MADE` token item through Stripe — it splits the
 * catalog into {@link ShopController.fiatItems} (packs / battle-pass / cosmetics
 * settled in real money) and {@link ShopController.tokenItems} (soft-currency),
 * and `checkout()` refuses anything that is not `settlement: 'fiat'`.
 *
 * Token / marketplace surfaces are additionally *capability-gated*: on a
 * native-shell build (`VITE_CAP_TOKEN=false`) the token items are withheld from
 * the controller entirely, so the view has nothing to render for them unless a
 * `redirectBaseUrl` is configured to web-redirect the buyer off-app.
 */
import { useCallback, useEffect, useMemo, useState } from 'react'
import { api, ApiError } from '../api'
import type { Order, ShopItem } from '../api/types'
import { capabilities } from '../config/capabilities'

/** Async loading lifecycle for the storefront fetch. */
export type LoadStatus =
  | { readonly phase: 'loading' }
  | { readonly phase: 'ready' }
  | { readonly phase: 'error'; readonly message: string; readonly retriable: boolean }

/** Everything the shop view needs to render the storefront and start a checkout. */
export interface ShopController {
  readonly status: LoadStatus
  /** Fiat items (packs / battle-pass / cosmetics) — always purchasable via Stripe. */
  readonly fiatItems: readonly ShopItem[]
  /**
   * `$MADE` token items, present only when the token capability is enabled for
   * this build (a native shell withholds them). Empty on a shell build.
   */
  readonly tokenItems: readonly ShopItem[]
  /** Whether the in-app token economy is available in this build/shell. */
  readonly tokenEnabled: boolean
  /** External URL a shell uses to web-redirect a disabled token flow (or `""`). */
  readonly redirectBaseUrl: string
  /** The SKU whose checkout is currently being opened, or `null`. */
  readonly pendingSku: string | null
  /** The last checkout error, or `null`. */
  readonly checkoutError: string | null
  /** Open a Stripe checkout for a fiat item and redirect the buyer to it. */
  readonly checkout: (item: ShopItem) => void
  /** Web-redirect a disabled token flow to the configured off-app storefront. */
  readonly redirectToTokenStore: () => void
  readonly reload: () => void
}

/** Navigate the browser to an absolute URL (guarded for SSR / test harnesses). */
function redirect(url: string): void {
  if (typeof window !== 'undefined') window.location.assign(url)
}

/**
 * Drive the shop view for `playerId`. When `playerId` is empty (no resolved
 * session yet) the controller stays in its loading phase and issues no request,
 * so the caller can render it unconditionally under the session gate.
 */
export function useShop(playerId: string): ShopController {
  const [status, setStatus] = useState<LoadStatus>({ phase: 'loading' })
  const [items, setItems] = useState<readonly ShopItem[]>([])
  const [pendingSku, setPendingSku] = useState<string | null>(null)
  const [checkoutError, setCheckoutError] = useState<string | null>(null)

  const [reloadNonce, setReloadNonce] = useState(0)
  const reload = useCallback(() => setReloadNonce((n) => n + 1), [])

  useEffect(() => {
    if (!playerId) return
    const ctrl = new AbortController()
    setStatus({ phase: 'loading' })
    api.shop
      .listItems({ signal: ctrl.signal })
      .then((list: readonly ShopItem[]) => {
        if (ctrl.signal.aborted) return
        setItems(list)
        setStatus({ phase: 'ready' })
      })
      .catch((err: unknown) => {
        if (ctrl.signal.aborted) return
        const e = err instanceof ApiError ? err : null
        setStatus({
          phase: 'error',
          message: e?.message ?? 'Failed to load the shop.',
          retriable: e?.retriable ?? true,
        })
      })
    return () => ctrl.abort()
  }, [playerId, reloadNonce])

  // Fiat items are always offered; token items are withheld entirely on a shell
  // build where the capability is off, so a native binary can never surface them.
  const fiatItems = useMemo(() => items.filter((i) => i.settlement === 'fiat'), [items])
  const tokenItems = useMemo(
    () => (capabilities.token ? items.filter((i) => i.settlement === 'token') : []),
    [items],
  )

  const checkout = useCallback(
    (item: ShopItem) => {
      if (!playerId) return
      // Defensive: Stripe settles fiat only — a token item must never reach it.
      if (item.settlement !== 'fiat') {
        setCheckoutError('That item is not purchasable with a card.')
        return
      }
      setPendingSku(item.sku)
      setCheckoutError(null)
      api.shop
        .createOrder({ playerId, lineItems: [item.sku], currency: item.currency })
        .then((order: Order) => {
          if (order.checkoutUrl) {
            // Hand off to the hosted Stripe Checkout Session.
            redirect(order.checkoutUrl)
            return
          }
          setCheckoutError('Checkout is temporarily unavailable. Please try again.')
          setPendingSku(null)
        })
        .catch((err: unknown) => {
          const e = err instanceof ApiError ? err : null
          setCheckoutError(e?.message ?? 'Failed to start checkout.')
          setPendingSku(null)
        })
    },
    [playerId],
  )

  const redirectToTokenStore = useCallback(() => {
    if (capabilities.redirectBaseUrl) redirect(`${capabilities.redirectBaseUrl}/shop/tokens`)
  }, [])

  return useMemo(
    () => ({
      status,
      fiatItems,
      tokenItems,
      tokenEnabled: capabilities.token,
      redirectBaseUrl: capabilities.redirectBaseUrl,
      pendingSku,
      checkoutError,
      checkout,
      redirectToTokenStore,
      reload,
    }),
    [status, fiatItems, tokenItems, pendingSku, checkoutError, checkout, redirectToTokenStore, reload],
  )
}
