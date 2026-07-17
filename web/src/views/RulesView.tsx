/**
 * Rules / How-to-Play — the living rulebook for MADE ("Rules of the City").
 *
 * This is the design source-of-truth surfaced in-product: the resource model
 * (Juice / Heat), the Cop Event swing, turn structure, card types, keywords,
 * and the two mechanics that set MADE apart from a battlecry game — **Heat
 * Trail** (lingering, triggered Heat) and **Street Events** (random Wildcard
 * consequences that fire off the card you play, the game's luck layer). It also
 * states the fair-play philosophy so balance intent is visible, not implicit.
 */
interface Section {
  title: string
  blurb: string
  items: { term: string; def: string }[]
}

const SECTIONS: readonly Section[] = [
  {
    title: 'Goal',
    blurb: 'Two crews, one city. Grind the rival boss from 30 down to 0 before they do it to you.',
    items: [
      { term: 'Boss HP', def: 'Each boss starts at 30. Reduce it to 0 to win the block.' },
      { term: 'Your crew', def: 'Operators fight; Jobs, Vehicles, Heists and Operations bend the board.' },
    ],
  },
  {
    title: 'Juice — your bankroll',
    blurb: 'Juice is what you spend to play cards. It is the tempo dial of the whole game.',
    items: [
      { term: 'Start', def: 'You begin the game with 1 Juice.' },
      { term: 'Growth', def: '+1 maximum Juice at the start of every turn, refilled each turn, cap 10.' },
      { term: 'Boss Power', def: 'Your boss’s signature power costs 2 Juice to fire.' },
    ],
  },
  {
    title: 'Heat — your wanted level',
    blurb: 'The signature risk meter. Loud, powerful plays raise Heat. Push too far and the city pushes back.',
    items: [
      { term: 'Range', def: 'Heat runs 0–10. Most big plays add +1 to +3 Heat.' },
      { term: 'Cop Event', def: 'Hit 10 Heat and the Cops raid: the highest-Heat player is punished, then Heat resets to 3.' },
      { term: 'Rubber-band', def: 'Because the raid hits the greediest player hardest, Heat is also a catch-up mechanic — the player behind gets a swing.' },
      { term: 'Cooling', def: 'The Police crew LOWERS Heat — the natural predator of a greedy deck.' },
    ],
  },
  {
    title: 'Heat Trail — Heat that lands later',
    blurb: 'Not all Heat is immediate. Heat Trail cards keep drawing attention as you keep operating — cheap and strong up front, they leak Heat over time.',
    items: [
      { term: 'Lingering', def: 'e.g. “Whenever you play a Vehicle, gain +1 Heat” — a payoff engine that slowly cooks you.' },
      { term: 'Draw-triggered', def: 'e.g. “Whenever you draw a Heist, gain 1 Heat” — your own deck raises the stakes.' },
      { term: 'Escalation', def: 'e.g. “Each Job after this gains +1 Heat” — big combo turns spike the meter toward the raid.' },
    ],
  },
  {
    title: 'Street Events — the Wildcard layer',
    blurb: 'What makes MADE not a battlecry game: some outcomes are NOT chosen. Certain plays roll a chance to trigger a random Street Event — emergent chaos, real luck.',
    items: [
      { term: 'Accidental Bite', def: 'Playing a K-9 can randomly bite a RANDOM Operator — friend or foe.' },
      { term: 'Dookie in the Vehicle', def: 'A Vehicle play can foul the ride — a random friendly Vehicle loses ATK this turn.' },
      { term: 'Why', def: 'Wildcards add variance skilled players can’t fully control, so no line of play is ever a sure thing.' },
    ],
  },
  {
    title: 'The turn',
    blurb: 'Five short phases, same every turn.',
    items: [
      { term: 'Refresh', def: 'Ready your board; ongoing effects tick.' },
      { term: 'Juice', def: 'Gain +1 max Juice and refill.' },
      { term: 'Draw', def: 'Draw one card (Street Events may roll here).' },
      { term: 'Main', def: 'Play cards, attack, fire your Boss Power.' },
      { term: 'End', def: 'End-of-turn effects; Heat Trail resolves.' },
    ],
  },
  {
    title: 'Card types',
    blurb: 'Six kinds of card, each with a role.',
    items: [
      { term: 'Operator', def: 'A body on the board (ATK / HP). Wins fights.' },
      { term: 'Job', def: 'A one-shot play — damage, removal, tempo.' },
      { term: 'Vehicle', def: 'Fast, aggressive bodies; often Rush.' },
      { term: 'Heist', def: 'Big swing plays — high cost, high payoff.' },
      { term: 'Operation', def: 'Ongoing engines that change the rules while in play.' },
    ],
  },
  {
    title: 'Keywords',
    blurb: 'The evergreen vocabulary printed on cards.',
    items: [
      { term: 'Spotlight', def: 'Enemies must deal with this first (taunt).' },
      { term: 'Stealth', def: 'Can’t be targeted until it attacks.' },
      { term: 'On Arrival', def: 'Triggers when it comes down.' },
      { term: 'Scout', def: 'Dig into your deck — selection that smooths luck.' },
      { term: 'Drive-By', def: 'Deals damage as it enters.' },
      { term: 'Cuff', def: 'Locks an enemy Operator — it can’t attack next turn.' },
    ],
  },
  {
    title: 'Deckbuilding & fair play',
    blurb: 'The rules that keep decks fair — power with a counter, and luck with skill.',
    items: [
      { term: '30 cards', def: 'An Outfit is exactly 30 cards.' },
      { term: 'Copy cap', def: 'At most 2 of any card; 1 of a Legendary. No deck draws its bomb every game.' },
      { term: 'Counter-triangle', def: 'Aggro / Control / Tempo / Combo each has a losing matchup — no deck holds a complete advantage.' },
      { term: 'Luck + skill', def: 'Draw variance, the Cop Event, and Street Events keep games live; Scout/selection lets skill smooth the swings.' },
    ],
  },
]

export default function RulesView() {
  return (
    <section className="rules" aria-labelledby="rules-title">
      <header className="rules__head">
        <h1 id="rules-title" className="rules__title">Rules of the City</h1>
        <p className="rules__sub">Juice to play. Heat to fear. The city always gets a vote.</p>
      </header>
      <div className="rules__grid">
        {SECTIONS.map((s) => (
          <article key={s.title} className="rules__card">
            <h2 className="rules__card-title">{s.title}</h2>
            <p className="rules__blurb">{s.blurb}</p>
            <dl className="rules__list">
              {s.items.map((it) => (
                <div key={it.term} className="rules__row">
                  <dt className="rules__term">{it.term}</dt>
                  <dd className="rules__def">{it.def}</dd>
                </div>
              ))}
            </dl>
          </article>
        ))}
      </div>
    </section>
  )
}
