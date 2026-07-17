# MADE — Production Game Design: Whole-Game Overview & Architecture

**Status:** Design / not yet implemented
**Date:** 2026-07-16
**Tracking issue:** edgentx/made-modernized-6c3820b4#108
**Companion spec:** [`2026-07-16-made-core-engine-design.md`](./2026-07-16-made-core-engine-design.md) — the implementation-ready spec for Subsystem #1, the first thing built.

> Audience: an engineer with strong Rust/DDD skills and **zero card-game-design context**. This document explains what the game *is*, the vocabulary, the design pillars, the cross-cutting engineering decisions, and the subsystem build order. It is deliberately not a code spec — the code spec for the first subsystem is the companion document. Where a term of art appears (mana, taunt, hero power, roguelike), it is defined inline.

---

## 1. Vision & north star

MADE is a **collectible card game (CCG)** in a **crime / heist** theme. Two players each field a criminal crew and try to defeat the other's boss. Today a thin, shallow version runs live at `https://dev.made.vforce360.ai/#/match` and on mobile browsers (verified on a Pixel). The goal is a **production-ready game that plays better than Hearthstone** — deeper rules, deeper cards, a real progression loop, tradeable card assets, and an AI opponent.

**North star: PvE roguelike-first.** The primary mode is single-player-versus-AI in the shape of *Slay the Spire × Hearthstone*:

- **Roguelike** = a run-based single-player structure. You start a "run," make a sequence of choices on a branching map, fight a ladder of increasingly hard AI opponents, and collect rewards between fights that make *this run* stronger. A run ends when you win the final boss or lose all your health; then you start a fresh run. Progression carries forward as unlocked content, not as a saved mid-run state.
- The player **picks a Boss (their hero)**, then **climbs a ladder** of mini-bosses → bosses, each an AI opponent (see §3.6). Between fights the player earns **trips / levels / idols** and new cards.
- **PvP comes later** on the *same engine* (see §4). Building PvE-first lets us get the rules, content, and feel right against an AI before we take on matchmaking, anti-cheat-under-adversarial-load, and competitive balance.

**"Take the best of every card game."** The design borrows deliberately: Hearthstone's turn/mana curve and board combat feel; Slay-the-Spire's run structure and relic collection; Magic/Hearthstone's rarity + rotation economy; and modern web3 CCGs' tradeable-asset ownership — but grounded, with a floor-fee economy that never loses money on a trade (see §3.5, §3.7).

The two differentiators we are betting on, beyond "deeper Hearthstone," are:
1. **City-as-map** — the board is a *place in a living city* with its own neutral effects and random events, and the roguelike map *is* the city (§3.1).
2. **A living, AI-authored card economy** — cards are tradeable assets that an AI pipeline continuously designs, versions, balances, and eventually rotates, keeping the game and the market perpetually fresh (§3.4, §3.5).

---

## 2. Theme & terminology (the crime/heist reskin)

The engine's domain vocabulary is a crime reskin of standard CCG concepts. This table is the Rosetta Stone; the rest of both documents uses the **MADE term**. These names are already baked into the Rust aggregates (`crates/game-session/src/lib.rs`, `crates/domain/src/*`).

| MADE term | Standard CCG concept | Notes / where it lives in code |
|---|---|---|
| **Juice** | Mana / energy — the per-turn resource you spend to play cards | `STARTING_JUICE=1`, `JUICE_CAP=10`, `JUICE_RAMP_PER_TURN=1` in `game-session/src/lib.rs`. **The ramp is currently bugged — see companion spec.** |
| **Outfit** | Hero + deck configuration (the crew you bring) | `OutfitConfig` struct; also the `outfit` bounded context (deck-building). |
| **Boss** | Hero — the unit you must kill to win; has a face HP total | `BossDefinition` aggregate; 18-boss authoritative roster. |
| **Heat** | A "wanted" meter that rises as you act; overflow triggers a punishment event | `HEAT_BOUNDS = 0..=10`; hitting 10 fires a **Cop Event**. |
| **Cop Event** | A board-wide punishment/random event when Heat maxes out | `ResolveCopEvent` command, seeded d10 table. |
| **Operators** | Minions / creatures — board units that attack and block | Board unit with atk/hp (today only client-side); `MAX_OPERATORS = 7`. |
| **Vehicles** | A second board-unit class (bigger units) | `MAX_VEHICLES = 3`. |
| **Job / Piece / Heist** | Spell-like card types | `CardType::{Operator, Job, Piece, Vehicle, Heist}`. |
| **Heist** | A high-cost payoff card gated behind a prerequisite queue | `outstanding_heist_prereqs`, `heist_resolved` on `OutfitConfig`. |
| **Class** | Faction / card allegiance | `CardClass::{Neutral, Boss, Muscle, Grifter, Hacker, Driver, Cleaner}`. |
| **Idol** | An artifact/relic that buffs your cards (e.g. +HP) across a run | Not yet in code — new (see §3.3, roguelike meta). |
| **Trip** | A run / streak in the roguelike ladder (a "job run") | Not yet in code — new. |
| **Relic** | A super-rare tradeable collectible dropped by activity | Not yet in code; will live near `card_token` (see §3.5). |
| **$MADE** | The in-game/on-chain currency | `emission_pool`, `marketplace_listing`, `order`. |

---

## 3. Design pillars

Each pillar below is a load-bearing product decision from Eric's 2026-07-16 brief. For each, this section states the design and flags which subsystem(s) own it. **Open numeric tuning is flagged, not resolved** (see §6).

### 3.1 City-as-map (the board is a place; the map is the city)

Replace Hearthstone's single static board with a **CITY**. Two ideas, unified:

**(a) Place-based venue modifiers.** Every encounter happens at a **location/point in the city**, and that location carries **neutral modifiers tied to the PLACE, not to either player** — they affect *both* sides symmetrically. Examples:
- *Server farm* → boosts Hacker-class cards.
- *Chop-shop / garage* → boosts Driver-class cards and Vehicles.
- *Bank* → warps Heist mechanics and intensifies the Cop/Heat response.
- *Rooftop, nightclub, docks* → their own flavored modifiers.

Because the modifier belongs to the venue and hits both players equally, it is a *neutral third actor*, not an advantage handed to one side. This is what makes the board interesting turn-over-turn without being unfair.

**(b) Venue random events.** Each venue also spawns **random events** (seeded RNG — the server already has `crates/server/src/ws/rng.rs`) with three outcome shapes:
- **Asymmetric one-off swing to one player** (positive) — e.g. a stashed cache.
- **Asymmetric one-off swing against one player** (negative) — e.g. a patrol, an alarm.
- **Shared persistent boon both players KEEP** — a lasting relic/idol carried *forward across the run*, roguelike-style. This is the seam where venue events feed the idol/trip progression (§3.3) and where relics can drop (§3.5).

**(c) Growing world (phased).** Launch **small** — one small district/map — and **grow the map as the game grows**, without a rewrite:
- Phase 1: one small area (a few locations).
- Later: larger map + more location types → **gang turfs → regions → cities → towns**, a nested, expanding geography with territory / gang-turf-control mechanics layered on top.
- **Engineering consequence:** locations must be **data-driven and hierarchical** from day one (a location has a parent, a type, a modifier set, and an event table), so scaling from one district to a nested world is *adding rows and tiers*, not restructuring.

**(d) Unification with the roguelike.** The **city map IS the roguelike map.** You traverse districts; each node on the run's branching map is a location; "you meet the mini-boss/boss at a point in the city," and that venue is the living board for that match. This is the single hardest differentiator vs Hearthstone's static board and leans directly into the crime theme + the `.148` SDXL location art + LivePortrait boss portraits.

**Owned by:** the **core engine** must expose a *location-modifier hook / seam* now (companion spec, §Location-modifier hook) so later subsystems fill in content without a rewrite; the **roguelike meta** subsystem owns city-map traversal; **presentation** owns the venue-rendered board.

### 3.2 Deeper rules & cards

The current rule set and card pool are deliberately shallow (a stub engine + ~14 client-only demo cards). Production must go substantially deeper on **both**:
- A **real keyword system** (keywords bound to engine behavior, not inert strings): today the client has `Spotlight` (taunt — must be attacked first) and `Drive-By` (arrival damage) as ad-hoc checks; the engine needs a first-class, extensible keyword mechanism.
- **Category-based abilities** — abilities keyed to a card's category/class (e.g. Hacker cards interact with Heat; Cleaner cards remove things). This is the depth layer that makes deck archetypes meaningful.
- **Card interactions / combos**, more nuanced targeting, and richer effects than the current closed 5-effect set (`damage/summon/draw/juice/cool`).

**Owned by:** the **core engine** lands the keyword/effect *machinery* (companion spec); the **content & abilities** subsystem authors the deep card pool on top of it.

### 3.3 Progression: trips, levels, idols, mini-bosses

Roguelike meta on top of matches:
- **Trips** — a run/streak up the ladder. Winning fights advances the trip; losing ends it. Rewards accrue per trip.
- **Levels** — persistent account/boss progression unlocked by playing trips.
- **Idols** — artifacts collected during a run that **buff your cards** (the canonical example: **+HP to your cards both on the board and in the deck**). Idols persist for the duration of a run and stack, the Slay-the-Spire "relic" analog *inside* a trip. (Distinct from tradeable **Relics** in §3.5, which are cross-run collectible assets.)
- **Mini-bosses → bosses** — the ladder rungs; each is an AI opponent of escalating difficulty (§3.6).

**Owned by:** the **roguelike meta** subsystem. Homes already scaffolded in `crates/domain`: `mission_attempt` (a clear attempt + first-clear reward), `ai_profile` (difficulty→strategy binding). New aggregates for trips/idols will join them.

### 3.4 Living AI content pipeline (a standing process)

A continual, AI-driven loop that keeps the card pool and economy fresh:
- **Designs new cards** and **card versions** — including **golden/premium** cosmetic variants and **stat-line variants** (a `1/1`, a `1/X`, …) — via **z.ai GLM** for design/text.
- **Generates art** via the `.148` **SDXL** (card art) + **LivePortrait** (animated boss portraits) pipeline (see `reference_148_art_motion_pipeline`).
- **Balances** each candidate through a **balance validator** (a `balance.json`-style automated check, per `project_made_card_game_build`) before it enters the pool.
- **Adds** new cards over time and eventually **RETIRES / rotates** old ones — a *living card game* with Standard-rotation semantics, keeping the metagame and the card market perpetually churning.

**Owned by:** spans **content & abilities** (the card schema + balance validator it targets), the **AI opponent** subsystem (shares the z.ai provider), and **economy** (rotation drives scarcity and market value). The card-authoring path already exists as `CardDefinition::{DefineCardCmd, ReviseCardCmd}` — the pipeline is an automated producer of those commands.

### 3.5 Super-rare relic economy

**Relics** are the premium tradeable collectibles of MADE. Distinct from idols (§3.3, run-scoped buffs), a relic is a scarce, ownable, tradeable **asset**.

- **Activity-gated drop rate.** Relic drop probability is a **percentage driven by aggregate live activity** — number of players playing, total hands played, total games played (global metrics, not just per-player RNG). Scarcity scales with the ecosystem so relics stay genuinely rare as the game grows. Drops are **seeded/auditable** so chain-of-custody is provable (it backs tradeable value).
- **Dual nature.** A relic can be **used in matches AND/OR traded peer-to-peer AND/OR sold on the marketplace.** Trade and sell are distinct liquidity paths (§3.7).
- **Rarity keyed to base commonality / faction.** A relic derives from either a **common** card or a **faction/class** type. Counter-intuitively, a **"common super-rare"** (a super-rare relic minted off a *common* base card) is the **most sought-after** — prestige from scarcity on a humble base. So value = *f(base commonality, faction, drop scarcity)*, not raw rarity tier alone.
- **Ownership/usage models** (each relic has exactly one):
  1. **Play + Display** — usable in matches *and* a showcase collectible.
  2. **Infinite plays** — never consumed.
  3. **Limited plays** — a finite charge count, then spent.
  4. **Decay / vanish** — **dies or disappears if you don't PLAY, TRADE, or SELL it** — use-it-or-lose-it, forcing circulation and punishing hoarding.

**Owned by:** **economy/marketplace**. Lives in `crates/domain` alongside `card_token` / `marketplace_listing` / `emission_pool`. The drop trigger is fed by the city random-event seam (§3.1b). Ties to idols/trips (a relic can drop from a venue event).

### 3.6 AI opponent (z.ai GLM)

The CPU boss is driven by **z.ai GLM** as an LLM decision-maker (which card to play, when to attack, when to use the hero power), reusing the **same z.ai provider** stood up for the lakehouse copilot (PR #748). Difficulty tiers map to strategies via the existing `ai_profile` bounded context: easy tiers can be **scripted/greedy** (the client already has a greedy `aiTurn` in `rules.ts` that plays the most expensive affordable card and swings everything — a perfectly good low-tier baseline), harder tiers escalate to **LLM-driven** search/decision with a move budget.

**Owned by:** the **AI opponent** subsystem. Because the engine is authoritative and server-side (§4), the AI runs server-side against the same `GameSession`, so it *cannot* cheat and is trivially swappable (scripted ↔ GLM) behind `ai_profile`.

### 3.7 Marketplace & fees

Cards (and relics) are **tradeable blockchain assets** (NFT-style ownership; ERC-1155 per `card_token`). Strategy:
- **OpenSea-first, own-market-eventually.** Integrate an existing marketplace (OpenSea) so trading works on day one, then build our **own marketplace** to stop paying external fees and to *waive/reduce* our fee when players trade on **our** market vs OpenSea (a fee incentive to use ours).
- **Fee model (decided):** **free trade + a small royalty, WITH a minimum fee floor.** Effective fee = **`max(min_fee_to_cover_cost, royalty_pct × price)`**:
  - The **floor** always covers the on-chain/gas transaction cost, so **we can never lose money on a trade.**
  - The **royalty follows the asset**, so our take grows as a card appreciates.
  - **Royalty rates (LOCKED):** **2.5%** on our own market, **5%** on external markets (OpenSea etc.). The lower own-market rate is the incentive to trade on ours; the external rate is set at the ERC-2981-typical ceiling so we still capture value when players use OpenSea. Both are always subject to the `max(floor, …)` guarantee.
- **ALL transfers are monetized.** Peer-to-peer *trades* also carry the small fee, not just marketplace *sells* — every transfer brings in a small amount above cost. No free transfers; the floor always covers the on-chain charge plus a bit.
- **Blockchain runs LOCAL until we move it to cloud** — this is the *one* component exempt from "everything is production/cloud." Market UX is production; the chain is local until stood up in cloud.

**Owned by:** **economy/marketplace**. Aggregates: `marketplace_listing` (P2P $MADE listings, ownership + settlement invariants), `card_token` (ERC-1155 mint, server-authoritative ownership), `order` (fiat via Stripe), `emission_pool` ($MADE reward pool), `battle_pass`.

---

## 4. Cross-cutting engineering decisions

These apply across all subsystems and constrain how each is built.

### 4.1 Unify rules onto the canonical Rust `game-session` engine

**The central architectural fix.** Today the *rich* gameplay rules — board units with atk/hp, summoning sickness, simultaneous combat + retaliation, the `Spotlight`/`Drive-By` keywords, effect resolution — live in the **non-authoritative TypeScript client** (`web/src/match/rules.ts`), while the **authoritative** Rust `crates/game-session` aggregate is a **stub**: it has no board entities, no card stats, and resolves "combat" by instantly killing the defending Boss (`declare_attack` sets `boss_hp = 0`).

This is backwards and unsafe. **Rules must be unified onto the Rust `game-session` crate as the single source of truth:**
- The Rust engine compiles to **WASM** (`wasm-pack build crates/game-session -- --features wasm`) and the **client runs the SAME rules via WASM for prediction** (optimistic UI, offline practice) — replacing the hand-maintained TS mirror in `rules.ts`.
- The **server re-runs every command authoritatively** through the same crate (`crates/server/src/ws/hub.rs` → `MatchHub::apply_action`), and its deltas reconcile the client's prediction.
- **Why server-authoritative matters here specifically:** cards are **tradeable assets with real value** (§3.5, §3.7). A client that can fabricate board state can fabricate value. Anti-cheat is non-negotiable, so the authority *must* be the server, and the client prediction *must* be the identical code (WASM) so predictions rarely diverge.

The companion spec (Subsystem #1) is exactly this port. It also fixes the **`AttackCmd` (client) vs `DeclareAttackCmd` (Rust)** command-drift that currently breaks online attacks.

### 4.2 WASM-on-mobile / PWA / touch is a first-class, non-regressing target

The production game **must run via WASM in a mobile browser with NO app store** — Eric verified it already runs on an Android Pixel. So the browser + WASM is the delivery vehicle, and this target must never regress:
- **PWA-installable** (add-to-home-screen).
- **Touch-optimized** controls (drag/drop cards by touch), responsive/mobile board layout.
- **Performant WASM** on mobile GPUs.

The `made-pwa` image already compiles the shared rules crate to WASM and Vite-builds the PWA (see README). Unifying rules onto Rust/WASM (§4.1) *strengthens* this: one rules artifact serves server, desktop, and mobile.

### 4.3 Everything is production/cloud except the blockchain

Per Eric: the **only** component that stays local until we explicitly stand it up in the cloud is the **blockchain**. Everything else (server, PWA, AI provider, market UX) targets production/cloud from the start.

---

## 5. The six subsystems & build order

The game is built in six subsystems, **in this order**. Each depends on the ones before it. The order front-loads correctness (a real, authoritative engine) before content, meta, AI, economy, and polish. The living AI content pipeline (§3.4) spans subsystems 2, 4, and 5.

### Subsystem 1 — Core canonical Rust engine  ← **build first; see companion spec**
**Scope (2–4 sentences):** Make `crates/game-session` the authoritative, complete rules engine. Fix the Juice-ramp bug (a real defect), port board units (atk/hp), summoning sickness, simultaneous combat + retaliation, a real keyword system, and effect resolution out of `rules.ts` into the aggregate; bind hero powers and boss-locked cards to real effects; resolve the `AttackCmd`/`DeclareAttackCmd` drift and make the WASM name-gate real; and add a location-modifier seam so the City pillar isn't a later rewrite.
**Maps to:** `crates/game-session/src/lib.rs` (aggregate + WASM), `crates/server/src/ws/hub.rs` (authoritative re-run), `crates/domain/src/{card_definition, boss_definition}.rs` (schema). Replaces the rules half of `web/src/match/rules.ts` with WASM calls.

### Subsystem 2 — Content & abilities
**Scope:** On the Subsystem-1 machinery, author the *deep* card pool: category-based abilities keyed to `CardClass`, a broad keyword catalog, card combos/interactions, and the balance validator the living AI pipeline (§3.4) targets. Grow far beyond the ~14 demo cards into archetype-supporting classes (Muscle/Grifter/Hacker/Driver/Cleaner).
**Maps to:** `crates/domain/src/card_definition.rs` (`DefineCardCmd`/`ReviseCardCmd`, `REGISTERED_EFFECTS`), the `content/` card data, and the keyword/effect registry landed in Subsystem 1.

### Subsystem 3 — Roguelike meta
**Scope:** The run structure: city-map traversal (the map *is* the city, §3.1d), mini-boss→boss ladders, **trips** (runs/streaks), **levels**, and **idols** (run-scoped card buffs, e.g. +HP). Between-fight reward choices and persistent unlock progression.
**Maps to:** `crates/domain/src/mission_attempt.rs` (clear attempts + first-clear rewards) and `ai_profile.rs` (difficulty→strategy); new aggregates for trips and idols; consumes the location hierarchy from Subsystem 1's seam.

### Subsystem 4 — AI opponent (z.ai)
**Scope:** Drive the CPU boss with z.ai GLM (reusing the PR-#748 provider), server-side against the authoritative `GameSession`. Difficulty tiers select scripted/greedy vs LLM-driven strategies via `ai_profile`. The greedy `aiTurn` from `rules.ts` is the low-tier baseline, ported alongside the engine.
**Maps to:** `crates/domain/src/ai_profile.rs`, a new server-side AI driver calling `GameSession::execute`, and the shared z.ai provider.

### Subsystem 5 — Economy & marketplace
**Scope:** Tradeable card/relic assets: OpenSea-first integration then our own market; the `max(floor, royalty)` fee on *all* transfers incl. P2P (§3.7); the super-rare relic economy with activity-gated drops and the four ownership models (§3.5). Blockchain local until cloud.
**Maps to:** `crates/domain/src/{card_token, marketplace_listing, emission_pool, order, battle_pass, player_collection, card_pack}.rs`; a new relic aggregate; relic-drop trigger fed by the city random-event seam.

### Subsystem 6 — Presentation
**Scope:** Hearthstone-grade feel: real drag/drop-onto-board animation, a polished, pleasing board, venue-rendered backgrounds (SDXL locations, LivePortrait bosses), and the non-regressing mobile-WASM/PWA/touch experience (§4.2). The board renders whatever authoritative state the engine produces, so it lands last, on a stable model.
**Maps to:** `web/` (React/TS PWA), the WASM rules artifact, the `.148` art/motion pipeline.

---

## 6. Open decisions to flag (do not block design)

These are intentionally deferred; they are tuning/timing, not architecture, and can be resolved during or after the relevant subsystem without reshaping it:

1. **Exact numeric tuning** — card stat lines, Juice costs per card, Boss HP totals within `LEGAL_STARTING_HP = 30..=90`, Heat-per-action beyond the base +1, idol buff magnitudes. (The *machinery* is specified; the *numbers* are balance work, and the living AI pipeline + balance validator will churn them continuously.)
2. **PvP timing & shape** — when PvP ladder arrives on the shared engine, and its matchmaking/ranked details (`matchmaking_ticket`, `ranked_standing`, `season` are scaffolded but out of the PvE-first critical path).
3. **Relic drop-rate formula parameters** — the exact activity→probability curve (§3.5). The *inputs* (players, hands, games) and the *shape* (activity-gated, seeded/auditable) are decided; the coefficients are tuning.
4. **Marketplace fork specifics** — the fee *model* AND rates are now decided (`max(floor, royalty)` on all transfers; royalty **2.5%** own-market / **5%** external — §3.7). Only the OpenSea→own-market cutover schedule remains open.

Everything above these four is settled design and can be built.
