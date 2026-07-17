/**
 * Marketplace — player-to-player card trading.
 *
 * Capability-gated (VITE_CAP_MARKETPLACE). A board of live listings: a card
 * (with its generated art), the asking price in $MADE, the seller, and rarity.
 * Buying spends $MADE (never fiat), keeping the token economy self-contained.
 * Demo listings reference real catalog art so the board reads like the game.
 */
interface Listing {
  cardId: string
  name: string
  rarity: 'common' | 'uncommon' | 'rare' | 'epic' | 'legendary'
  price: number
  seller: string
}

const LISTINGS: readonly Listing[] = [
  { cardId: 'pd_swat_captain', name: 'SWAT Captain', rarity: 'legendary', price: 3200, seller: 'NightRunner' },
  { cardId: 'w_the_plug', name: 'The Plug', rarity: 'rare', price: 900, seller: 'V0LT' },
  { cardId: 'ht_the_come_up', name: 'The Come-Up', rarity: 'epic', price: 1500, seller: 'CipherJack' },
  { cardId: 'w_trigger_man', name: 'Trigger Man', rarity: 'epic', price: 1400, seller: 'NeonFox' },
  { cardId: 'pd_flashbang', name: 'Flashbang', rarity: 'rare', price: 850, seller: 'Sable' },
  { cardId: 'w_donk_on_dubs', name: 'Donk on Dubs', rarity: 'uncommon', price: 400, seller: 'Rook' },
  { cardId: 'w_the_big_one', name: 'The Big One', rarity: 'epic', price: 1650, seller: 'Static' },
  { cardId: 'pd_dirty_cop', name: 'Dirty Cop', rarity: 'epic', price: 1300, seller: 'NeonFox' },
  { cardId: 'w_casino_heist', name: 'Casino Heist', rarity: 'rare', price: 780, seller: 'V0LT' },
  { cardId: 'ht_going_loud', name: 'Going Loud', rarity: 'rare', price: 720, seller: 'NightRunner' },
  { cardId: 'w_ride_or_die', name: 'Ride or Die', rarity: 'uncommon', price: 350, seller: 'Rook' },
  { cardId: 'pd_riot_squad', name: 'Riot Squad', rarity: 'rare', price: 690, seller: 'CipherJack' },
]

export default function MarketplaceView() {
  return (
    <section className="market" aria-labelledby="market-title">
      <header className="market__head">
        <h1 id="market-title" className="market__title">Marketplace</h1>
        <p className="market__sub">Player-to-player trades. Priced in $MADE — no cash changes hands.</p>
      </header>
      <ul className="market__grid" aria-label="Card listings">
        {LISTINGS.map((l) => (
          <li key={l.cardId} className={`market__listing market__listing--${l.rarity}`}>
            <span
              className="market__art"
              aria-hidden="true"
              style={{ backgroundImage: `url(/assets/cards/${l.cardId}.webp)` }}
            />
            <div className="market__info">
              <span className="market__name">{l.name}</span>
              <span className="market__rarity">{l.rarity}</span>
              <span className="market__seller">@{l.seller}</span>
            </div>
            <button type="button" className="market__buy">
              <span className="market__price">{l.price.toLocaleString()}</span>
              <span className="market__cur">$MADE</span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  )
}
