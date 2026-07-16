# MADE — Subsystem 2: Content & Abilities — Design Spec

> **Companion to:** `2026-07-16-made-game-overview-design.md` (§3.2, §3.4, §5 Subsystem 2) and `2026-07-16-made-core-engine-design.md` (Subsystem 1, the machinery this builds on). Base branch: `feat/core-engine` @ `55dbabe` (PR #110).

**Status:** Draft for Eric's approval.

---

## 1. Goal

On the Subsystem-1 authoritative Rust engine, turn the deliberately-shallow rule set (a closed 5-effect set, a 2-keyword enum, a 5-card domain catalog) into a **depth layer with real archetypes**: category-based abilities keyed to `CardClass`, a broad keyword catalog bound to engine behavior, richer effects, card combos, a single authoritative card catalog, and a CI-gated balance validator. And it lands the **foundational per-turn card draw** deferred out of Subsystem 1 — with true hidden-information handling (you see the card you drew; your opponent sees only that you drew).

## 2. The three scope decisions (flagged for approval)

These are the calls where your answer changes the shape of the work. I've made a recommendation on each; say the word to redirect any of them.

### 2.1 Seed pool, not the final metagame  ← **the scope limiter**

Subsystem 2 builds the **machinery** (effects, keywords, class identities, combos, the validator) and authors an **archetype-complete _seed_ pool** — enough cards to (a) give each of the five classes a coherent, playable identity and (b) exercise every keyword and every effect at least once. Target **~60 cards** (≈10 per class + neutrals), not a balanced, shipped 300-card metagame.

**Why:** the overview (§3.4, §6.1) explicitly hands continuous card generation and numeric tuning to the **living AI content pipeline**. If Subsystem 2 hand-authors the whole final pool, we (a) duplicate what the AI pipeline exists to do and (b) spend this subsystem on balance-number churn instead of the machinery that makes any pool meaningful. The seed pool is the *exemplar set* the AI pipeline learns the schema and archetype shapes from — the same "hand-code one exemplar, then teach the generator" pattern used elsewhere in the platform.

### 2.2 One authoritative catalog  ← **consolidation**

There are two divergent catalogs today:
- `content/catalog/cards.json` — **5 cards**, the domain `DefineCardCmd` schema, **CI-gated** by the Rust `content-validator` (runs the real `CardDefinition::execute` aggregate path).
- `services/data/cards.json` — **156 cards**, a *different, non-conforming* schema (`cardType`/`cardClass`, classes like `"Solomon"`, empty `effectScriptRef`, lowercase rarities), validated only by `services/data/validate.mjs` (Node, **not in CI**), consumed by the frontend demo.

**Recommendation:** collapse to **one** authoritative catalog under `content/catalog/`, on the domain schema, extended with **optional presentation fields** (`art`, `text`) so the frontend renders from the same file the rules validate. Port `validate.mjs`'s balance checks into a **Rust balance validator** (§7) so balance is CI-gated in the same place as content. Retire `services/data/cards.json` + `validate.mjs`; salvage art paths/flavor from the demo pool where cards map. This is the §4.1 "single source of truth" philosophy applied to content: the rules, the runtime, and the UI all read one file.

### 2.3 Per-turn draw is a real transport change  ← **foundational, do it first**

Subsystem 1 left start-of-turn draw a no-op (`resolve_start_of_turn_draw` is `&self`, computes fatigue off the scalar `deck_size`, and never moves `SeatState.deck → hand`). Fixing the *draw* is small. Making it **hidden-information-correct** is not: the server broadcasts **one identical frame to every connection** (a single `broadcast::Sender` per match) — there is **no per-recipient framing anywhere**. A true "you see your card, opponent sees only that you drew" requires new hub plumbing so the drawn card's identity never travels to the opponent's connection at all (client-side "receive-then-hide" is not hidden info — a modded client reads the frame).

**Recommendation:** build this as **Part 1**, the foundation. It's the first true server-side secret in the game; getting the per-recipient seam right now means idols, mulligan reveals, and any future hidden state reuse it instead of each re-inventing it.

---

## 3. Architecture overview

Five parts, in dependency order. Parts 1–3 are *machinery* (engine + transport + registry); Parts 4–5 are *content* (the pool + the validator gate) built on that machinery.

```
Part 1  Hidden per-turn draw          game-session: CardDrawn event + real draw
        + per-recipient transport     server/ws: per-seat framing, masked secrets
                                       web: fold full + masked card.drawn
        ─────────────────────────────────────────────────────────────────────
Part 2  Effect catalog expansion      game-session: CardEffect + from_script_ref
                                       domain: REGISTERED_EFFECTS (kept in sync)
Part 3  Keyword catalog + class        game-session: Keyword enum + parse + hooks
        identity mechanics             class-conditional effect resolution
        ─────────────────────────────────────────────────────────────────────
Part 4  Combos / card interactions     game-session: per-turn play tracking +
                                       Combo effect wrapper
Part 5  Seed card pool (~60)           content/catalog/*.json (one schema)
        + Rust balance validator       crates/domain/src/bin/balance-validator.rs
                                       + CI wiring; retire services/data catalog
```

**Invariants preserved from Subsystem 1:**
- `crates/game-session` stays WASM-safe (only `shared` + serde/serde_json, optional `wasm` feature). No new host-only deps.
- `REGISTERED_EFFECTS` (domain, the CI-validated allow-list) and `CardEffect::from_script_ref` (game-session, the behavior map) must stay **in sync** — every registered ref resolves to a real effect or an explicitly-commented no-op.
- Every mutation the client must observe emits an **event** with a delta arm (the "silent effect mutation" lesson from Subsystem 1).
- The domain `validate_card_fields` remains the single hard-invariant gate (spell-vs-body stats, cost ranges, one-class rule, boss-lock, legendary copy-cap).

---

## 4. Part 1 — Hidden per-turn draw & per-recipient transport

### 4.1 The draw itself (game-session)

- New event `CardDrawn { match_id, player_id, player: Player, card: CardInstance }` (event_type `"card.drawn"`), added to the `Event` enum. It carries the **full** `CardInstance` — the server decides per-recipient what to reveal (§4.3); the event is the authoritative record.
- `end_turn` calls a new `draw_for_turn(incoming)` **before** the fatigue path: if `SeatState.deck` is non-empty, `draw_one(incoming)` moves top→hand and emits `CardDrawn`; if empty, the existing fatigue path runs (emits `FatigueDamageDealt`). Draw and fatigue are mutually exclusive per turn (you draw a card OR you take fatigue).
- **Reconcile the scalar/vector drift:** `OutfitConfig.deck_size` and `SeatState.deck.len()` currently diverge (`deck_size` is never decremented on draw). Make `deck_size` a **derived** read (`self.seat_state_at(seat).deck.len()`) wherever fatigue/UI needs a count, and stop treating the scalar as authoritative. The `Vec<CardInstance>` is the single source of truth for deck contents *and* size.
- First turn: the opening player does **not** draw on the match's first `end_turn` into their own first turn if Subsystem-1 opening-hand rules already dealt them a starting hand — confirm against `start_match`/`OPENING_HAND` so we don't double-deal. (Standard rule: opening player draws normally each of their turns; the going-second bonus, if any, is a tuning number deferred to §6.1.)

### 4.2 Deck order visibility

The deck is **server-secret and ordered**. In online play the client does **not** know its own deck order, so the owner learns their drawn card only from the server. (Practice/offline mode runs the full WASM session locally and already knows the deck — see §4.4.)

### 4.3 Per-recipient framing (server/ws/hub.rs)

Replace the single shared `broadcast::Sender<String>` fan-out with **per-connection delivery keyed by seat**, so a delta can be *masked* for recipients who must not see it:

- Each `LiveMatch` tracks its connected seats → per-connection senders (an `mpsc` per connection, or a seat→sender map). Public deltas go to **all** connections unchanged (the common case — combat, plays, heat, turn-ended are all public).
- A delta carries an **audience**: `Public` (everyone, default) or `SeatOnly(Player)` (only that seat's connections) paired with a `Masked` public variant for the others.
- `CardDrawn` framing:
  - **Owner's connection(s):** full frame — `{type:"card.drawn", player, card:{cardId,name,cost,...atk,hp,keywords,text}}`.
  - **Everyone else:** masked frame — `{type:"card.drawn", player, hidden:true, deckSize:<n>}` (no card identity ever serialized to the opponent).
- The **backlog** (`live.deltas`, replayed to late joiners) stores the **public/masked** form of secret deltas, never the secret. A spectator or reconnecting opponent must never be able to reconstruct a hidden card from history.

This is the minimum plumbing for hidden information; it is deliberately general (any future `SeatOnly` secret — mulligan, idol reveals — reuses it).

### 4.4 Client fold (web/src/match)

- `model.ts`: the `card.drawn` delta becomes a **discriminated** shape — either `{player, card: HandCard}` (owner / practice) or `{player, hidden:true, deckSize:number}` (opponent).
- `rules.ts` `foldEvent`: full form moves deck→hand + decrements `deckSize` (existing behavior); masked form only decrements the opponent's `deckSize` and pushes a **face-down placeholder** into their hand model (hand-size stays truthful; identity unknown).
- Practice/offline mode keeps emitting the **full** `card.drawn` locally from the WASM session (it owns the deck), so nothing regresses offline. Only the *online server path* masks.

### 4.5 Tests (Part 1)

- game-session unit: `end_turn` on a non-empty deck moves exactly one card deck→hand and emits `CardDrawn`; on an empty deck emits `FatigueDamageDealt` and no `CardDrawn`; `deck_size`-derived count matches `deck.len()` after N draws.
- hub: a `CardDrawn` produces a **full** frame to the drawing seat and a **masked** frame (no `card`/`cardId`) to the other seat; backlog contains only the masked form.
- client: folding the masked form advances opponent hand-size + deckSize without leaking identity; folding the full form reveals the card.

---

## 5. Part 2 — Effect catalog expansion

Extend the closed 5-effect set into a catalog rich enough to express the class identities (§6). Each new effect gets **(a)** a `REGISTERED_EFFECTS` entry (domain allow-list, CI-checked), **(b)** a `CardEffect` variant + `from_script_ref` arm (game-session behavior), and **(c)** an emitted event + delta arm for any state change the client must see.

Two refs already registered-but-no-op (`effect.steal_piece`, `effect.pull_heist`) get **real** implementations here.

New effect set (final variant list settled in writing-plans; magnitudes are `#[serde(default)]` amounts carried on the `CardInstance`, per §6.1 tuning-deferred):

| Script ref | Effect | Behavior |
|---|---|---|
| `effect.noop` … `effect.cool` | *(existing 5)* | unchanged |
| `effect.steal_piece` | `StealPiece` | move a Piece/attachment from opponent to you (Grifter) |
| `effect.pull_heist` | `PullHeist` | draw-then-play or tutor a specific card (Hacker finisher) |
| `effect.raise_heat` | `RaiseHeat{amount}` | push own or enemy Heat up (Hacker tempo/burn) |
| `effect.buff_unit` | `BuffUnit{atk,hp}` | permanent +atk/+hp to a board unit (idol-shaped; reused by Subsystem 3) |
| `effect.aoe_damage` | `AoeDamage{amount}` | damage all enemy operators (Cleaner board-clear) |
| `effect.destroy` | `Destroy` | remove a target operator outright (Cleaner removal) |
| `effect.silence` | `Silence` | strip keywords + pending effects from a unit (Cleaner answer) |
| `effect.gain_armor` | `GainArmor{amount}` | Boss armor (already an event in Subsystem 1) |
| `effect.summon_token` | `SummonToken{card_id,count}` | summon N copies of a token card (Muscle/Driver go-wide) |

Targeting: effects that need a target consume a `target_ref` from the play command payload (the `target_ref` seam already exists from Subsystem-1 combat). Untargeted/AoE effects ignore it. Illegal targets → `InvariantViolation`.

---

## 6. Part 3 — Keyword catalog & class identity

### 6.1 Keyword catalog (game-session `Keyword` enum + `parse` + hooks)

Today only `Spotlight` (taunt) and `Drive-By` (arrival damage) parse — yet catalog cards already reference `Recruit`, `Wheels`, `Shielded`, `Overload`, etc., which `Keyword::parse` currently **rejects** (a latent load failure). Subsystem 2 makes the catalog real:

| Keyword | Semantics | Hook point |
|---|---|---|
| `Spotlight` | must be attacked before other units *(exists)* | combat target legality |
| `Drive-By` | deal arrival damage on summon *(exists)* | on-summon |
| `Charge` (a.k.a. `Wheels`) | may attack the turn it arrives (no summoning sickness) — **Driver identity** | readying/attack legality |
| `Stealth` | cannot be targeted or attacked until it attacks — resolves the deferred `w_corner_boy` follow-up | combat + effect target legality |
| `Shielded` | ignore the first instance of damage, then break — **defensive** | damage application |
| `Recruit` | summon an operator on play — **Muscle/Driver go-wide** | on-play effect |
| `Overload{n}` | costs `n` Juice next turn — **Hacker power-at-a-price** | end-of-turn Juice bookkeeping |
| `Payout` | trigger an effect when this unit dies (deathrattle) | on-death |
| `Signature` | Boss-locked / one-per-deck marker (interacts with boss-lock + copy-cap) | deck-build validation |

Each keyword is a first-class enum variant with an explicit `parse` arm and one clearly-named hook in the engine — no ad-hoc string checks. `Silence` (§5) strips keywords, so keyword storage on `BoardUnit` must be mutable at runtime.

### 6.2 Category-based abilities keyed to `CardClass`

The depth layer that makes decks *archetypes* rather than stat piles. Each class has a **signature mechanic** — a bias in which effects/keywords its cards carry, plus (where warranted) a small class-conditional resolution rule in the engine:

- **Muscle** — raw board pressure. Big bodies, `Recruit`, `SummonToken`; **Overkill**: excess combat damage from a Muscle attacker spills to the Boss. Aggression.
- **Grifter** — tempo & theft. `StealPiece`, bounce, Juice generation; cheap disruptive bodies. Value from taking what's yours.
- **Hacker** — Heat & cards. `RaiseHeat`, `PullHeist`, `Overload` combos, card advantage. The engine/combo class (§7).
- **Driver** — speed. `Charge`/`Wheels`, cheap fast units, reach. Wins before the opponent sets up.
- **Cleaner** — removal & control. `AoeDamage`, `Destroy`, `Silence`, `GainArmor`. The answer class.
- **Neutral** — archetype-agnostic staples usable in any deck (baseline bodies, generic effects).

Class-conditional rules stay **minimal and explicit** (e.g. Overkill is one check in combat resolution gated on `attacker.class == Muscle`). We do not build a general "class trigger engine" — YAGNI; the identities are expressed mostly through *which* effects/keywords each class's cards carry in the seed pool.

---

## 7. Part 4 — Combos & card interactions

The "card interactions" pillar (§3.2), scoped tightly:

- **Per-turn play tracking:** `SeatState` gains a `cards_played_this_turn: u8` (reset in `end_turn` when a seat's turn begins). This is the one piece of cross-card state combos need.
- **`Combo` effect wrapper:** a card may carry a `Combo{ base: Box<CardEffect>, bonus: Box<CardEffect> }` (script ref `effect.combo`). On play, `base` always resolves; `bonus` resolves **additionally** iff `cards_played_this_turn >= 1` at the moment of play (i.e. this isn't the first card you played this turn) — the classic Hearthstone Combo trigger. Hacker's identity leans on this.
- Because effects already resolve through one `resolve_effect` dispatcher (Subsystem 1), `Combo` is just a dispatcher arm that conditionally recurses — no new resolution machinery.
- Keyword-driven interactions (`Payout` on death, `Overload` next-turn cost) are the *other* half of "interactions" and live in Part 3's hooks; Part 4 adds only the play-count-conditioned combo.

**Tests:** playing a `Combo` card first in a turn resolves base only; playing it after another card resolves base + bonus; `cards_played_this_turn` resets each turn.

## 8. Part 5 — Seed card pool & balance validator

### 8.1 The seed pool (~60 cards, one schema)

- Authored as JSON under `content/catalog/` on the **domain schema** (`DefineCardCmd` camelCase), extended with optional presentation fields `art` (string path) and `text` (rules text). ~10 cards per class + a neutral staple set, each card deliberately exercising a keyword and/or effect from Parts 2–3 so the pool is a **complete machinery exercise**, not a metagame.
- Every card passes the existing domain `validate_card_fields` (spell-vs-body stats, cost range, one class, boss-lock ⇒ Boss, legendary ⇒ copyCap 1) — enforced by the CI `content-validator`, unchanged.
- The two-catalog reconciliation (§2.2): retire `services/data/cards.json` + `validate.mjs`; the frontend loads the unified `content/catalog` set. Salvage art paths from the demo pool where a demo card maps to a seed card.

### 8.2 Rust balance validator (CI-gated, data-driven)

Port `validate.mjs` into a Rust binary `crates/domain/src/bin/balance-validator.rs` that reads `balance.json` (the existing tuning file — `statBudget`, `cost`, `deck`, `answerDensity`, `starterDecks`) and enforces, over the unified catalog:

- **Structural:** duplicate `cardId`; `cost` within `[cost.min, cost.max]`; `copyCap ≤ cap` (legendary vs default).
- **Stat budget (WARN):** for `statBudget.appliesTo` types with bodies, `atk + hp > slope*cost + base` ⇒ outlier warning.
- **Answer density (ERROR if required):** at least one direct-damage answer exists in the pool (regex over rules `text`), per `answerDensity`.
- **Starter-deck parity (WARN):** each starter deck is `deck.size` cards of known ids; average-cost spread across decks within `starterDecks.avgCostSpreadMax`.

Exit non-zero on ERROR, advisory on WARN — same contract as `validate.mjs`, but in Rust and **wired into CI** next to `content-validate`. This is the automated gate the living AI pipeline (§3.4) submits candidate cards through: it reads `balance.json`, so balance stays data-driven and re-tunable without recompiling.

**Tests:** a stat-line over budget WARNs; a pool with no direct-damage answer ERRORs when required; a duplicate id ERRORs; a balanced pool exits 0.

## 9. What Subsystem 2 does NOT do (YAGNI / deferred)

- **Final metagame balance / full card list** — the living AI pipeline (§3.4) and §6.1 tuning own this. Seed pool only.
- **Art/animation generation** — Subsystem 6 + the `.148` pipeline. Seed cards reference art paths; generating the art is not this subsystem.
- **Idols / run buffs** — Subsystem 3 (though `BuffUnit` (§5) is deliberately shaped so idols reuse it).
- **AI opponent card evaluation** — Subsystem 4; the AI *plays* these cards but its decision logic is separate.
- **A general class-trigger engine** — class identity is expressed through card composition + a few explicit conditional rules, not a rules framework.

## 10. Testing strategy

- **game-session** (Rust unit, `cargo test`): every new effect resolves + emits its event; every keyword hook fires at the right seam; combo condition; hidden-draw deck/hand movement.
- **domain** (Rust unit): `REGISTERED_EFFECTS`/`from_script_ref` sync test (every registered ref maps to a non-`None`-by-accident effect or an explicitly-commented no-op); `validate_card_fields` on new field combinations.
- **content-validator + balance-validator** (CI binaries): the unified seed pool passes both; deliberately-broken fixtures fail as specified.
- **server/ws** (Rust): per-recipient framing masks `CardDrawn` for the opponent; backlog stores only masked secrets.
- **web** (`vitest src/match`): fold of full vs masked `card.drawn`; no regression in existing 25 match tests.
- **WASM parity:** the extended engine still builds to WASM (`--features wasm`) and the bit-exact parity test passes.

## 11. Global constraints (carried into the plan)

- `crates/game-session` stays WASM-safe: `shared` + serde/serde_json only (+ optional `wasm` feature). No host-only deps.
- `REGISTERED_EFFECTS` (domain) and `from_script_ref` (game-session) stay in sync — enforced by a sync test.
- No silent mutation: every client-observable state change emits an event **and** a hub delta arm.
- Hidden information is enforced at the **transport** boundary (opponent's connection never receives the secret), never by client-side hiding.
- One authoritative catalog on the domain schema; balance is data-driven from `balance.json` and CI-gated.
- Terminology stays MADE-canonical (Juice/Outfit/Boss/Heat/Operators/Pieces/Heists/Jobs; classes Muscle/Grifter/Hacker/Driver/Cleaner/Neutral).
- No `Co-Authored-By` / "Generated with" trailers in any commit; issue + PR for the work.
- Base branch `feat/core-engine` @ `55dbabe` (stacked on unmerged PR #110); all anchors against it.

## 12. Build order & handoff

Implementation splits along the Part boundaries, most likely as **two plans** (both stacked on `feat/core-engine`):
- **Plan 2A — Foundation & machinery:** Parts 1–4 (hidden draw + transport, effect catalog, keyword catalog + class hooks, combos). Produces a strictly-more-capable engine with the seam ready; independently testable.
- **Plan 2B — Content & gate:** Part 5 (seed pool + Rust balance validator + catalog reconciliation + CI wiring). Depends on 2A's registry being complete.

Each plan produces working, tested software on its own and is reviewed via subagent-driven-development, per the Subsystem-1 flow.
