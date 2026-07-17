/**
 * Bosses gallery — the 19 Crown City crew bosses as *living portraits*.
 *
 * Each portrait is a short looping clip produced by the .148 content pipeline
 * (SDXL portrait → LivePortrait face reenactment driven by a neutral→smile
 * clip), so the characters visibly smile / shift on the page. The `<video>` is
 * muted + autoplay + loop + playsInline (the combination browsers allow to
 * autoplay), with the static portrait as `poster` so the tile is never blank
 * while the clip loads.
 *
 * Metadata (name / tagline / class / boss power) mirrors `heroes()` in
 * `made-site/src/data.rs` — the canonical roster.
 */
// Bumped when the living-portrait clips are regenerated. The mp4s live at
// stable URLs served with a 1-year immutable cache, so this query param is what
// forces browsers to refetch the new render instead of the cached old one.
const CLIP_VERSION = 2

interface Boss {
  id: string
  name: string
  tagline: string
  cls: string
  power: string
}

const BOSSES: readonly Boss[] = [
  { id: 'nimrod', name: 'Nimrod II', tagline: 'The Tower-Builder. Sells the future, ships it late.', cls: 'Ramp / Builder', power: 'Tower' },
  { id: 'cain', name: 'Cain Akaw', tagline: 'The First Murderer, still angry, still on the move.', cls: 'Aggro Burn', power: 'Mark' },
  { id: 'solomon', name: 'Solomon Vault', tagline: 'The Logistics King. Wisest in the warehouse.', cls: 'Midrange Value', power: 'Pull Order' },
  { id: 'cleo', name: 'Cleo Reign', tagline: 'The Empire Queen. A court of Operators that feed her sun.', cls: 'Swarm Empire', power: 'Royal Court' },
  { id: 'moshe', name: 'Moshe Stone', tagline: 'The Demo Prophet. Holds the Tablet. Knows the future.', cls: 'Combo Control', power: 'The Tablet' },
  { id: 'mags', name: 'Mags the Bapt', tagline: 'The Wilderness Preacher. Returns your sins to your hand.', cls: 'Spell-heavy', power: 'Repent' },
  { id: 'homestead', name: 'Lady Homestead', tagline: 'The Camera-Loved. Smile-for-cereal, eyes that burn.', cls: 'Aggro Powerhouse', power: 'Smile for the Camera' },
  { id: 'judas', name: 'Judith Coin', tagline: 'The Rug-Puller. Sells you the future, pockets the bag.', cls: 'Disruption', power: 'Thirty Pieces' },
  { id: 'pokka', name: 'Pippa Starr', tagline: 'The Critter-Wrangler. A satchel of small loyal beasts.', cls: 'Tempo Curve', power: 'Wrangle' },
  { id: 'zhrrx', name: 'Ambassador Zhrrx', tagline: 'The Diplomat From Elsewhere. Has been watching us.', cls: 'Disruption', power: 'First Contact' },
  { id: 'bunnay', name: 'Jack Hare', tagline: 'The Short-Seller. Plays your Operators twice.', cls: 'Trickster Combo', power: 'Right Back' },
  { id: 'daff', name: 'Dorian Duval', tagline: 'The Failed Producer. Bleeds for the pitch.', cls: 'Self-harm Aggro', power: "It's Mine!" },
  { id: 'chase', name: 'The Chase', tagline: 'Two stances. One body. Endless rivalry.', cls: 'Duo Disruption', power: 'Switch' },
  { id: 'lionnay', name: 'Leona Mgaba', tagline: 'The Warrior-Queen. Sword across her back, pride on the line.', cls: 'Midrange Weapon', power: 'Eye of the Pride' },
  { id: 'adam', name: 'Eve Powerlock', tagline: 'Two forms. Default Eve, then by her word — Powerlock.', cls: 'Powerhouse Transform', power: 'By My Word' },
  { id: 'mick', name: 'Marv Sterling', tagline: 'The Empire-Owner. Mascots in his colors. Nothing personal.', cls: 'Swarm Leader', power: 'Empire Hire' },
  { id: 'clyde', name: 'CL-7N “Clyde”', tagline: 'The Synthetic Twin. Plays your deck back at you.', cls: 'Disruption Copy', power: 'Mirror Match' },
  { id: 'hexx', name: 'Hexx-Ellen-ia', tagline: 'The Suburban Curse. Ticks down on what you love.', cls: 'Spell-heavy', power: 'Hex Bag' },
  { id: 'crowe', name: 'Hollis Crowe', tagline: 'The Loudmouth. Talks over everyone, buries you in noise.', cls: 'Aggro / Heat', power: 'Hold the Floor' },
]

export default function HeroesView() {
  return (
    <section className="bosses" aria-labelledby="bosses-title">
      <header className="bosses__head">
        <h1 id="bosses-title" className="bosses__title">Crown City Bosses</h1>
        <p className="bosses__sub">Nineteen crews run this city. Pick your fight.</p>
      </header>
      <ul className="bosses__grid" aria-label="Boss roster">
        {BOSSES.map((b) => (
          <li key={b.id} className="boss">
            <div className="boss__portrait">
              <video
                className="boss__video"
                src={`/assets/living/${b.id}.mp4?v=${CLIP_VERSION}`}
                poster={`/assets/heroes/${b.id}.webp`}
                autoPlay
                loop
                muted
                playsInline
                preload="metadata"
                aria-label={`${b.name} living portrait`}
              />
              <span className="boss__power">{b.power}</span>
            </div>
            <div className="boss__meta">
              <span className="boss__name">{b.name}</span>
              <span className="boss__class">{b.cls}</span>
              <span className="boss__tagline">{b.tagline}</span>
            </div>
          </li>
        ))}
      </ul>
    </section>
  )
}
