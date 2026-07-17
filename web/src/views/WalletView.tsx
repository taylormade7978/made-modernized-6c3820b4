/**
 * Wallet — on-device balances + a transaction ledger.
 *
 * Capability-gated (VITE_CAP_WALLET). A plain statement view over the player's
 * currencies: the earned $MADE soft currency and fiat spend settled through the
 * Shop's Stripe path. Read-only; top-ups happen in the Shop, earning in Match.
 */
const BALANCES = [
  { cur: '$MADE', amount: '4,820', hint: 'Soft currency — earned in-game' },
  { cur: 'USD', amount: '$0.00', hint: 'Fiat is charged per Shop order, not held' },
]

const LEDGER = [
  { date: '2026-07-07', desc: 'Daily first win', kind: 'earn', amount: '+150 $MADE', bal: '4,820' },
  { date: '2026-07-07', desc: 'Ranked victory', kind: 'earn', amount: '+50 $MADE', bal: '4,670' },
  { date: '2026-07-06', desc: 'Chrome Ghost skin', kind: 'spend', amount: '−1,200 $MADE', bal: '4,620' },
  { date: '2026-07-06', desc: 'Neon Heist Pack (Stripe)', kind: 'fiat', amount: '−$1.99', bal: '—' },
  { date: '2026-07-05', desc: 'Story: cleared Solomon Vault', kind: 'earn', amount: '+300 $MADE', bal: '5,820' },
  { date: '2026-07-05', desc: 'Season 1 Battle Pass (Stripe)', kind: 'fiat', amount: '−$14.99', bal: '—' },
]

export default function WalletView() {
  return (
    <section className="econ" aria-labelledby="wallet-title">
      <header className="econ__head">
        <h1 id="wallet-title" className="econ__title">Wallet</h1>
        <p className="econ__sub">Balances and every transaction, in one ledger.</p>
      </header>
      <div className="econ__balances">
        {BALANCES.map((b) => (
          <div key={b.cur} className="econ__bal-chip">
            <span className="econ__bal-cur">{b.cur}</span>
            <span className="econ__bal-amt">{b.amount}</span>
            <span className="econ__bal-hint">{b.hint}</span>
          </div>
        ))}
      </div>
      <div className="econ__ledger" role="table" aria-label="Transaction history">
        <div className="econ__ledger-head" role="row">
          <span role="columnheader">Date</span>
          <span role="columnheader">Description</span>
          <span role="columnheader">Amount</span>
          <span role="columnheader">Balance</span>
        </div>
        {LEDGER.map((l, i) => (
          <div key={i} className="econ__ledger-row" role="row">
            <span role="cell" className="econ__ledger-date">{l.date}</span>
            <span role="cell">{l.desc}</span>
            <span role="cell" className={`econ__ledger-amt econ__ledger-amt--${l.kind}`}>{l.amount}</span>
            <span role="cell" className="econ__ledger-bal">{l.bal}</span>
          </div>
        ))}
      </div>
    </section>
  )
}
