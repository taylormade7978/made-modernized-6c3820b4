import { useSession } from '../auth/SessionProvider'
import { useShop } from '../shop/useShop'
import type { ShopItemKind } from '../api/types'

/**
 * Shop view.
 *
 * Lists the storefront (card packs, the battle-pass, cosmetics) and starts a
 * **Stripe fiat checkout** for a chosen item via {@link useShop}. Per the shop
 * payments policy this client only ever initiates fiat orders through Stripe —
 * `$MADE` token purchases are never routed through it. Token surfaces are
 * capability-gated: on a native-shell build they are withheld entirely, or (when
 * a redirect base URL is configured) offered only as an off-app web redirect,
 * satisfying the app-store crypto/wallet restrictions.
 *
 * The layout is mobile-first: a single scrolling column of stacked cards on a
 * phone, widening to a responsive card grid on a tablet+ viewport.
 */
export default function ShopView() {
  const { state } = useSession()
  const playerId =
    state.status === 'ready' && state.session.authenticated ? state.session.identity.subject : ''
  const shop = useShop(playerId)

  if (shop.status.phase === 'loading') {
    return (
      <section className="shop" aria-labelledby="shop-title">
        <h1 id="shop-title" className="shop__title">Shop</h1>
        <p className="shop__status" role="status" aria-live="polite">Loading the shop…</p>
      </section>
    )
  }

  if (shop.status.phase === 'error') {
    return (
      <section className="shop" aria-labelledby="shop-title">
        <h1 id="shop-title" className="shop__title">Shop</h1>
        <p className="shop__error" role="alert">{shop.status.message}</p>
        {shop.status.retriable ? (
          <button type="button" className="shop__btn" onClick={shop.reload}>Try again</button>
        ) : null}
      </section>
    )
  }

  return (
    <section className="shop" aria-labelledby="shop-title">
      <h1 id="shop-title" className="shop__title">Shop</h1>

      {shop.checkoutError ? (
        <p className="shop__error" role="alert">{shop.checkoutError}</p>
      ) : null}

      {/* ── Fiat storefront: packs, battle-pass, cosmetics ─────────────────── */}
      <h2 className="shop__section-title">Packs &amp; Battle Pass</h2>
      <ul className="shop__grid" aria-label="Purchasable items">
        {shop.fiatItems.map((item) => (
          <li key={item.sku} className="shop__card">
            <div className="shop__card-head">
              <span className={`shop__kind shop__kind--${item.kind}`}>{kindLabel(item.kind)}</span>
              <span className="shop__price">{formatPrice(item.priceMinor, item.currency)}</span>
            </div>
            <h3 className="shop__card-name">{item.name}</h3>
            <p className="shop__card-desc">{item.description}</p>
            <button
              type="button"
              className="shop__btn shop__btn--primary"
              onClick={() => shop.checkout(item)}
              disabled={shop.pendingSku !== null}
              aria-label={`Buy ${item.name}`}
            >
              {shop.pendingSku === item.sku ? 'Opening checkout…' : 'Buy'}
            </button>
          </li>
        ))}
        {shop.fiatItems.length === 0 ? (
          <li className="shop__empty">Nothing for sale right now — check back soon.</li>
        ) : null}
      </ul>

      <TokenSection shop={shop} />
    </section>
  )
}

/**
 * The `$MADE` token storefront section.
 *
 * On a build where the token capability is enabled it lists the soft-currency
 * items (bought via the in-app token economy, never Stripe). On a native shell
 * that disables the capability it renders nothing — unless a `redirectBaseUrl`
 * is configured, in which case it offers a single web-redirect off-app instead
 * of an in-app purchase surface.
 */
function TokenSection({ shop }: { shop: ReturnType<typeof useShop> }) {
  if (!shop.tokenEnabled) {
    if (!shop.redirectBaseUrl) return null
    return (
      <div className="shop__token-redirect">
        <h2 className="shop__section-title">$MADE Tokens</h2>
        <p className="shop__card-desc">Token purchases are handled on the web.</p>
        <button type="button" className="shop__btn" onClick={shop.redirectToTokenStore}>
          Open token store
        </button>
      </div>
    )
  }

  if (shop.tokenItems.length === 0) return null

  return (
    <>
      <h2 className="shop__section-title">$MADE Tokens</h2>
      <ul className="shop__grid" aria-label="Token items">
        {shop.tokenItems.map((item) => (
          <li key={item.sku} className="shop__card shop__card--token">
            <div className="shop__card-head">
              <span className="shop__kind shop__kind--token">token</span>
              <span className="shop__price">{item.priceMinor} $MADE</span>
            </div>
            <h3 className="shop__card-name">{item.name}</h3>
            <p className="shop__card-desc">{item.description}</p>
            {/* Token items never go through Stripe; the in-app token flow owns them. */}
            <a className="shop__btn" href="/token">Use tokens</a>
          </li>
        ))}
      </ul>
    </>
  )
}

/** Human label for a shop item kind. */
function kindLabel(kind: ShopItemKind): string {
  switch (kind) {
    case 'pack':
      return 'pack'
    case 'battlePass':
      return 'battle pass'
    case 'cosmetic':
      return 'cosmetic'
    case 'expansion':
      return 'expansion'
  }
}

/** Format a minor-unit fiat price (e.g. 499 USD → "$4.99"). */
function formatPrice(priceMinor: number, currency: string): string {
  const major = priceMinor / 100
  try {
    return new Intl.NumberFormat(undefined, { style: 'currency', currency }).format(major)
  } catch {
    // Unknown/invalid currency code — fall back to a plain amount + code.
    return `${major.toFixed(2)} ${currency}`
  }
}
