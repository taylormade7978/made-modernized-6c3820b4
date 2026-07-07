/**
 * Tokens — the $MADE soft-currency economy.
 *
 * Capability-gated (VITE_CAP_TOKEN); absent from native-shell builds per the
 * app-store crypto/wallet policy. $MADE is never bought with fiat (that is the
 * Shop's Stripe path) — it is *earned* in-game and spent on cosmetics. This
 * screen shows the balance, the earn rates, the sinks, and recent activity.
 */
const BALANCE = 4_820

const EARN = [
  { label: 'Win a ranked match', amount: '+50', note: 'per victory' },
  { label: 'Daily first win', amount: '+150', note: 'resets 00:00 UTC' },
  { label: 'Story boss first-clear', amount: '+300', note: 'one-time each' },
  { label: 'Battle-pass tier', amount: '+100', note: 'every tier' },
]
const SPEND = [
  { label: 'Card-back cosmetic', amount: '−500' },
  { label: 'Boss skin', amount: '−1,200' },
  { label: 'Board theme', amount: '−800' },
]
const ACTIVITY = [
  { when: 'Today 02:14', what: 'Daily first win', delta: '+150' },
  { when: 'Today 01:58', what: 'Ranked victory vs NightRunner', delta: '+50' },
  { when: 'Yesterday', what: 'Chrome Ghost skin', delta: '−1,200' },
  { when: 'Yesterday', what: 'Story: cleared Solomon Vault', delta: '+300' },
  { when: '2 days ago', what: 'Battle-pass tier 12', delta: '+100' },
]

export default function TokenView() {
  return (
    <section className="econ" aria-labelledby="token-title">
      <header className="econ__head">
        <h1 id="token-title" className="econ__title">$MADE Tokens</h1>
        <p className="econ__sub">Earned on the street, spent on the drip. Never bought with cash.</p>
      </header>
      <div className="econ__balance">
        <span className="econ__balance-label">Balance</span>
        <span className="econ__balance-amount">{BALANCE.toLocaleString()} <em>$MADE</em></span>
      </div>
      <div className="econ__cols">
        <article className="econ__card">
          <h2 className="econ__card-title">Ways to earn</h2>
          <ul className="econ__rows">
            {EARN.map((e) => (
              <li key={e.label} className="econ__row">
                <span className="econ__row-main">{e.label}<small>{e.note}</small></span>
                <span className="econ__row-amt econ__row-amt--pos">{e.amount}</span>
              </li>
            ))}
          </ul>
        </article>
        <article className="econ__card">
          <h2 className="econ__card-title">Spend it on</h2>
          <ul className="econ__rows">
            {SPEND.map((s) => (
              <li key={s.label} className="econ__row">
                <span className="econ__row-main">{s.label}</span>
                <span className="econ__row-amt econ__row-amt--neg">{s.amount}</span>
              </li>
            ))}
          </ul>
        </article>
        <article className="econ__card">
          <h2 className="econ__card-title">Recent activity</h2>
          <ul className="econ__rows">
            {ACTIVITY.map((a, i) => (
              <li key={i} className="econ__row">
                <span className="econ__row-main">{a.what}<small>{a.when}</small></span>
                <span className={`econ__row-amt ${a.delta.startsWith('+') ? 'econ__row-amt--pos' : 'econ__row-amt--neg'}`}>{a.delta}</span>
              </li>
            ))}
          </ul>
        </article>
      </div>
    </section>
  )
}
