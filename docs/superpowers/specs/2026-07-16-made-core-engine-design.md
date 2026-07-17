# MADE — Core Engine Design (Subsystem #1, the first thing built)

**Status:** Design / implementation-ready
**Date:** 2026-07-16
**Tracking issue:** edgentx/made-modernized-6c3820b4#108
**Companion spec:** [`2026-07-16-made-game-overview-design.md`](./2026-07-16-made-game-overview-design.md) — vision, pillars, terminology, subsystem map. **Read its §2 (terminology) and §4.1 (engine unification) first.**

> Audience: an engineer with strong Rust/DDD skills and zero card-game context. This is a single, implementable slice: make `crates/game-session` the authoritative, complete rules engine. It fixes one real bug, ports the rich rules out of the TypeScript client into the Rust aggregate, and lands the seams later subsystems need. Every mechanic below is specified with concrete rules and numbers — no TBDs. Line anchors reference the code as of `origin/main` @ `c4466ff`.

## Scope of this document (and what is explicitly out)

**In scope (Subsystem #1):**
1. **Juice-crystal fix** — a real bug fix to the per-turn resource ramp.
2. **Engine unification** — port board units, combat, keywords, and effects from `web/src/match/rules.ts` into `crates/game-session` as the authoritative model; the client calls the same code via WASM.
3. **Card & board model** — add play-stats (atk/hp) to cards; reconcile catalog vs client card types; make keywords semantic.
4. **Hero powers with real effects** — bind Boss `hero_power`/`trademark` to actual effects.
5. **Boss-locked cards** — a real "only this Boss may deck this card" mechanic.
6. **Command-drift fix** — reconcile `AttackCmd` (client) vs `DeclareAttackCmd` (Rust) and make the WASM name-gate real.
7. **Location-modifier hook** — the engine seam where a venue's modifiers + random events apply (content lands later, but the seam ships now so the City pillar isn't a rewrite).

**Out of scope (later subsystems):** the deep card pool (Subsystem 2), trips/idols/mini-boss ladder (3), the z.ai AI driver (4), marketplace/relics (5), animation/presentation (6). This doc lands the *machinery* those build on.

---

## 0. Current-state map (what exists today)

- **`crates/game-session/src/lib.rs`** — the aggregate. It has commands `StartMatchCmd, MulliganCmd, PlayCardCmd, DeclareAttackCmd, ActivateHeroPowerCmd, EndTurnCmd, ResolveCopEventCmd, ConcedeMatchCmd`. But its state is only `OutfitConfig` *scalars* (`operators: usize`, `vehicles: usize`, `deck_size: usize`, `boss_hp`, `starting_heat`, `starting_juice`, `available_juice`, `heist_resolved`, `outstanding_heist_prereqs`) — **no hand, no deck contents, no board entities, no card instances with atk/hp.** Combat is a stub: `declare_attack` (lib.rs:1333) validates the defender names the enemy Boss, then sets `boss_hp = 0` and emits `CombatResolved` + `BossDefeated`. Playing a card (`play_card`, lib.rs:1260) emits `CardPlayed` + `HeatRaised` but **never deducts Juice and never writes Heat to state** (see §1).
- **`web/src/match/rules.ts`** — the *rich* rules, client-side and non-authoritative: `BoardUnit` with `atk/hp/maxHp/ready/keywords`, `CARD_POOL` (~14 cards with atk/hp/effect), simultaneous combat with retaliation (`applyAction` `AttackCmd` arm, rules.ts:229), `Spotlight` (taunt) and `Drive-By` (arrival damage) keywords, a 5-kind effect resolver (`damage/summon/draw/juice/cool`), and a greedy `aiTurn`. This is the behavior to port.
- **`crates/server/src/ws/hub.rs`** — `MatchHub::apply_action(match_id, command_name, payload)` (hub.rs:271) re-runs the client's command authoritatively via `build_command` → `GameSession::execute`, broadcasts deltas, and returns a correction on rejection. The server envelope (`ClientMessage { type:"action", matchId, command, payload }`, `ws/mod.rs`) already carries a JSON payload — so the server side of the protocol is real.
- **`web/src/match/connection.ts`** — the client transport. **`send(action)` currently ships only `action.kind`** (the bare command-name string, connection.ts:68) — no matchId, no payload — so the rich server envelope is not actually being used yet. This is half of the command-drift problem (§6).
- **`crates/game-session` WASM** — `wasm_bindings::execute_command(session_id, command_name)` (lib.rs:1798) builds a **fresh empty** `GameSession` and runs a **bare-named, payload-less** `Command` against it. It proves the crate loads in WASM but validates nothing real (§6).

DDD conventions to preserve throughout: every aggregate embeds `AggregateRoot`, implements `Aggregate::execute(Command) -> Result<Vec<Event>, DomainError>`, routes on the command *name*, returns `DomainError::UnknownCommand` for anything unrecognized, and records events via `self.root.record(...)`. Commands carry an opaque JSON payload (`Command::with_payload`), decoded with `serde_json`. Every mechanic here is a command→event(s) transition tested by asserting the emitted events and the resulting state. **This crate must stay WASM-safe: only `shared` + `serde`/`serde_json`, no host-only deps outside the `wasm`-gated module.**

---

## 1. Juice fix (a real bug — call it out)

### The bug
Juice is meant to ramp like Hearthstone mana: you start with 1, and at the start of each of your turns your maximum grows by 1 (capped at 10) **independent of how much you spent last turn**, then your available pool refills to that maximum. Today it does neither:

1. **No separate max-Juice crystal.** `OutfitConfig` has only `available_juice` (lib.rs:183). `end_turn` ramps it with `ramped_juice` (lib.rs:1485) = `available_juice.saturating_add(1).min(10)` — it adds to the **remaining** pool. So if you spend down to 0, next turn you get `0 + 1 = 1`, and you are **pinned at 1 Juice forever**. There is no memory of "you should have N max this turn."
2. **`play_card` never deducts Juice.** `play_card` (lib.rs:1260) calls `ensure_card_affordable` (a read-only check, lib.rs:1226) and `heat_after_play` (also read-only, lib.rs:1244), then emits `CardPlayed`/`HeatRaised` — but **never mutates `available_juice` or `starting_heat`**. Compare `activate_hero_power`, which *does* deduct: `outfit.available_juice -= cmd.juice_cost` (lib.rs:1467). So playing cards is free and raises no persisted Heat; only the hero power spends.

Net effect: the resource curve is broken (stuck at 1) and card plays don't cost anything server-side. The client `rules.ts` *does* model spend/ramp correctly, which is why it "looks" fine in offline practice and diverges from the server.

### The fix

**Introduce a per-seat max-Juice crystal that grows each of the owner's turns, independent of spend; refill available to max at turn start; make `play_card` actually deduct Juice and apply Heat to state.**

**(a) New aggregate field.** Add to `OutfitConfig` (lib.rs:157):
```rust
/// The seat's max-Juice "crystal": the ceiling `available_juice` refills to at
/// the start of each of the owner's turns. Grows by `JUICE_RAMP_PER_TURN` each
/// of the owner's turns, hard-capped at `JUICE_CAP`, INDEPENDENT of spend.
pub max_juice: u8,
```
`OutfitConfig::new` sets `max_juice: STARTING_JUICE` (=1) and `available_juice: STARTING_JUICE`. (The current `new` sets `available_juice: 3` "a few turns in" for test convenience — keep a constructor that yields a legal opening, but the *canonical* opening is `max=available=1`; tests that need a mid-game seat set both explicitly.) Extend `ensure_starting_juice_valid` (lib.rs:970) to also assert `max_juice <= JUICE_CAP` and, at a clean start, `max_juice == STARTING_JUICE`.

**(b) Turn-start sequence.** Replace `ramped_juice` (lib.rs:1485) with a crystal ramp applied to the **incoming** seat inside `end_turn` (lib.rs:1513). The incoming seat's start-of-turn, in order:
1. **Grow the crystal:** `max_juice = (max_juice + JUICE_RAMP_PER_TURN).min(JUICE_CAP)`.
2. **Refill available:** `available_juice = max_juice`.
3. Resolve the start-of-turn draw (existing `resolve_start_of_turn_draw`, lib.rs:1498) — unchanged.
4. Ready the incoming seat's Operators (clear summoning sickness — see §2).

Emit a new/expanded event carrying **both** numbers so the client can render the crystal:
```rust
pub struct TurnEnded {
    // ...existing fields...
    pub next_player_max_juice: u8,      // NEW: the grown crystal
    pub next_player_juice: u8,          // now == next_player_max_juice (the refill)
}
```
This is the concrete bug fix: because the crystal grows independent of spend and available refills *to the crystal* (not `spent+1`), a seat that emptied its pool still gets its full grown allotment next turn.

**(c) `play_card` must mutate state.** In `play_card` (after the affordability + heat checks, around lib.rs:1307), before recording events:
```rust
let outfit = self.outfit_at_mut(seat);
outfit.available_juice -= cmd.juice_cost;   // deduct Juice (checked affordable above)
outfit.starting_heat = new_heat;            // persist the Heat raise (was computed, never stored)
```
Then apply the card's actual effect to board/hand/Boss state (§2/§3) and record `CardPlayed` + `HeatRaised` (+ any effect events). `new_heat` is already computed by `heat_after_play` (lib.rs:1244); today it is only put in the event, not the state — this line is the fix.

> Rename note: `OutfitConfig.starting_heat` and `.starting_juice` are misnomers once the aggregate is live state (they are "current heat"/"opening juice"). Optional cleanup: rename `starting_heat` → `heat`. Not required for correctness; if done, do it as a mechanical rename in its own commit.

### Numbers (fixed, from the brief)
`STARTING_JUICE = 1`, `JUICE_CAP = 10`, `JUICE_RAMP_PER_TURN = 1`, `HEAT_BOUNDS = 0..=10`, `HEAT_PER_PLAY = 1`. All already declared in lib.rs:106–139; the crystal reuses them.

### Test strategy (DDD command→event)
- `end_turn_grows_incoming_crystal_and_refills_available`: seat with `max=3, available=0` → after opponent ends turn, `TurnEnded.next_player_max_juice == 4` and `next_player_juice == 4`. **This is the regression test for the pin-at-1 bug** — assert that a fully-spent pool refills to the grown crystal, not to 1.
- `crystal_caps_at_ten`: `max=10` → stays 10 after ramp.
- `play_card_deducts_juice`: `available=5`, play a cost-3 card → seat `available_juice == 2`.
- `play_card_persists_heat`: `heat=0` → after play, seat `starting_heat == 1` (state, not just event).
- `play_card_rejects_when_cost_exceeds_available` already exists (lib.rs:2315) — keep; add the post-state assertion that a rejected play leaves `available_juice` unchanged.

---

## 2. Engine unification: port the rich rules into `game-session`

Port the authoritative model out of `web/src/match/rules.ts` into `crates/game-session`. After this, `rules.ts`'s *rules* are deleted and the client calls the WASM engine (§6); `rules.ts` keeps only view-concerns (none of the mutation logic).

### 2.1 New aggregate state the domain currently lacks

`GameSession` today holds two `OutfitConfig` scalar bags. Introduce real per-seat live state. Add a `SeatState` inside the aggregate (kept WASM-safe: plain structs, `serde`):

```rust
/// A card instance in a hand or deck: a definition ref + per-copy identity + play-stats.
pub struct CardInstance {
    pub instance_id: String,   // e.g. "A-w_the_homie-3"
    pub card_id: String,       // definition id
    pub cost: u8,
    pub card_type: CardType,   // Operator/Job/Piece/Vehicle/Heist (see §3.2)
    pub effect: CardEffect,    // resolved effect (see §3.3)
    pub atk: u8,               // 0 for non-unit cards
    pub hp: u8,                // 0 for non-unit cards
    pub keywords: Vec<Keyword>,// semantic keywords (see §3.4)
    pub boss_lock: Option<String>, // Some(boss_id) if boss-locked (see §5)
}

/// A unit on the board (summoned Operator or Vehicle).
pub struct BoardUnit {
    pub instance_id: String,
    pub card_id: String,
    pub atk: u8,
    pub hp: u8,
    pub max_hp: u8,
    pub ready: bool,           // false the turn it arrives (summoning sickness)
    pub is_vehicle: bool,      // counts against MAX_VEHICLES vs MAX_OPERATORS
    pub keywords: Vec<Keyword>,
}

/// Live per-seat state (replaces the scalar counters on OutfitConfig for a live match).
pub struct SeatState {
    pub outfit_name: String,
    pub boss_id: String,
    pub boss_hp: i32,
    pub heat: i32,
    pub max_juice: u8,
    pub available_juice: u8,
    pub hand: Vec<CardInstance>,
    pub deck: Vec<CardInstance>,   // server-secret; ordered
    pub board: Vec<BoardUnit>,
    pub heist_resolved: bool,
    pub outstanding_heist_prereqs: usize,
}
```
`GameSession` holds `SeatState` for A and B plus `turn: Option<Player>`, `match_id`, `rng_seed`, and the seeded RNG cursor. `OutfitConfig` remains the *opening* configuration input to `StartMatchCmd`; `start_match` deals the opening hand from the seeded deck (mirroring `rules.ts` `startMatch`/`buildDeck`) and constructs the two `SeatState`s. Keep board caps: `MAX_OPERATORS = 7`, `MAX_VEHICLES = 3` (lib.rs:109–112), enforced when a unit is summoned.

Board caps enforcement: on summon, reject if `board.iter().filter(|u| !u.is_vehicle).count() >= MAX_OPERATORS` (Operators) or the Vehicle equivalent `>= MAX_VEHICLES`.

### 2.2 Combat (simultaneous + retaliation), ported from `rules.ts:229`

`declare_attack` (currently the boss-instakill stub at lib.rs:1333) becomes real combat over `BoardUnit`s. Rules (numbers/behavior copied from the client so prediction == authority):
- Attacker must be a **ready** unit the acting seat owns; a unit is not ready the turn it arrives (summoning sickness) and becomes ready at the owner's next turn-start (§1b step 4).
- Target is either the enemy Boss (`boss:<seat>`) or an enemy unit (`op:<instance_id>`), mirroring the client's `targetRef` scheme.
- **Spotlight (taunt):** if the defending seat has any unit with `Keyword::Spotlight`, the attack must target one of those units (rules.ts:169). Reject otherwise.
- **Simultaneous resolution:** capture `attacker.atk` and `defender.atk` **before** applying damage (rules.ts:231–235). Attacker deals `atk` to the target; if the target is a unit, it **retaliates** its `atk` back to the attacker. Apply both, then remove any unit at `hp <= 0`.
- Attacker exhausts (`ready = false`) if it survives.
- If the target was the Boss, reduce `boss_hp`; if it reaches 0, the match ends (winner = attacker's seat).

**Events** (replace the stub's `CombatResolved`+`BossDefeated` with granular deltas that already have client foldEvent handlers in `model.ts`/`rules.ts`): `operator.damaged { player, instanceId, newHp }`, `operator.died { player, instanceId }`, `boss.damaged { player, amount, newHp }`, `operator.exhausted { player, instanceId }`, and `match.completed { concedingPlayer, winner }` (or a dedicated `BossDefeated`) when a Boss hits 0. Keep the existing `Event` enum but **add** these variants and their `event_type()` strings so `DomainEvent::event_type` (lib.rs:820) stays exhaustive. The client already understands these exact delta types (`model.ts:223`), so the server just needs to emit them.

### 2.3 Effect resolution, ported from `rules.ts:299`

Playing a card resolves its effect against state (not just emit a bare `CardPlayed`). Port the client's `resolveEffect`: `damage` (to target), `summon` (put a `BoardUnit` on the board, `ready=false`), `juice` (gain Juice this turn, capped), `cool` (lower own Heat), `draw` (take N from deck to hand). Extend to a richer set in Subsystem 2 — but land this closed set now so `play_card` produces real board changes. `summon` must enforce board caps (§2.1) and apply arrival keywords (Drive-By, §3.4).

---

## 3. Card & board model

### 3.1 Add play-stats (atk/hp) to the card catalog

`CardDefinition` (crates/domain/src/card_definition.rs) today carries `cost, class, card_type, rarity, keywords, effect_script_ref, copy_cap` — **no atk/hp**. Add an optional **play-stats** concept:
```rust
// on DefineCardCmd / ReviseCardCmd / CardDefined / CardRevised:
#[serde(default)] pub atk: u8,   // 0 for non-unit types
#[serde(default)] pub hp: u8,    // 0 for non-unit types
```
Invariant: `Operator` and `Vehicle` cards **must** have `hp >= 1` (a unit needs health to exist); `Job`/`Piece`/`Heist` must have `atk == 0 && hp == 0` (they are spell-like — no board body). Enforce in `validate_card_fields` (card_definition.rs:285) alongside the existing per-type cost ranges (`legal_cost_range`, card_definition.rs:78). This keeps illegal states unrepresentable at the catalog boundary. The `game-session` `CardInstance` (§2.1) is populated from these fields when a deck is built.

### 3.2 Reconcile card types (fix the Piece vs Operation drift)

The catalog `CardType` enum is `{Operator, Job, Piece, Vehicle, Heist}` (card_definition.rs:51). The client `CARD_POOL` uses type strings `'Operator' | 'Job' | 'Piece' | 'Vehicle' | 'Heist'` — **but two demo cards use `type: 'Operation'`** (`pd_the_crib`, `ht_the_come_up` in rules.ts:59–60), which is **not** a catalog type. Resolve by making the Rust `CardType` the single source of truth (five types, no `Operation`) and **retyping those two client cards to `Piece`** (the catalog's spell-like non-unit type). After the port, the client no longer defines card types at all — it consumes them from the engine/catalog — so this drift cannot recur.

### 3.3 Card effects: a resolvable enum, not an opaque string ref

Today the catalog stores an `effect_script_ref: String` validated against a `REGISTERED_EFFECTS` allow-list (card_definition.rs:39). The engine needs an effect it can *execute*. Introduce a `CardEffect` enum in `game-session` mirroring the client's closed set, with the catalog `effect_script_ref` mapping onto it:
```rust
pub enum CardEffect {
    None,
    DealDamage { amount: u8 },
    Summon,                    // stats come from the CardInstance's atk/hp
    DrawCards { amount: u8 },
    GainJuice { amount: u8 },
    Cool { amount: u8 },       // lower own Heat
    // extended in Subsystem 2
}
```
`REGISTERED_EFFECTS` stays the catalog's validation allow-list; add a total mapping `effect_script_ref -> CardEffect` used at deck-build time. Every registered effect must map (no partial coverage), enforced by a test that iterates `REGISTERED_EFFECTS`.

### 3.4 Make keywords semantic (bound to engine behavior, not inert strings)

Today keywords are `Vec<String>` on the catalog and ad-hoc `.includes('Spotlight')` checks on the client. Introduce a real keyword system:
```rust
pub enum Keyword {
    Spotlight,   // taunt: enemy attacks must target a Spotlight unit first
    DriveBy,     // on arrival, deal this unit's `atk` (or a fixed amount) to the enemy Boss
    // extended in Subsystem 2
}
```
Bind behavior in the engine: `Spotlight` is checked in combat targeting (§2.2); `DriveBy` fires in the summon path (§2.3), mirroring rules.ts:312. Catalog `keywords: Vec<String>` is parsed into `Vec<Keyword>` at deck-build (unknown keyword strings rejected, like `CardType::parse`). The *machinery* is the deliverable here; the *catalog* of keywords grows in Subsystem 2.

---

## 4. Hero powers with real effects

Each Boss has a **hero power** — a repeatable ability activated for Juice (Hearthstone's hero power). Today `BossDefinition.hero_power` and `.trademark` are **inert strings** (boss_definition.rs:121–123), and the client hardcodes a flat 2-damage poke (rules.ts:250). Bind them to real effects.

**(a) `activate_hero_power` already deducts Juice** (lib.rs:1467) — that half is correct. Extend it to **apply the effect**. Model a Boss's hero power as a typed effect on `BossDefinition`:
```rust
// on BossDefinition / DefineBossCmd:
pub hero_power: String,          // display name (kept)
pub hero_power_effect: HeroPowerEffect,  // NEW: what it does
pub hero_power_cost: u8,          // NEW: Juice cost (Hearthstone canonical = 2)

pub enum HeroPowerEffect {
    DealDamage { amount: u8 },     // e.g. 2 to a target
    GainArmor { amount: u8 },      // add to Boss hp
    SummonToken { atk: u8, hp: u8 },
    Cool { amount: u8 },
    // extended in Subsystem 2
}
```
`activate_hero_power` (lib.rs:1418): keep the turn/affordability checks, keep the `available_juice -= cost` deduction, then **resolve `hero_power_effect` against `target_ref`** (reusing the §2.3 effect machinery) and emit the resulting delta events (`boss.damaged`, `operator.summoned`, …) **in addition to** `HeroPowerActivated`. The cost comes from `hero_power_cost` (default 2), not a client-supplied `juice_cost` — validate the command's cost matches the Boss's declared cost so the client can't understate it.

**(b) Trademark = a passive/triggered signature.** The `trademark` (the Boss's second inert string) becomes a passive effect that triggers on a defined hook (e.g. start-of-turn, or on-play). Model it as `trademark_effect: TrademarkEffect` with a `trigger: TrademarkTrigger` enum. For Subsystem 1, land the field + one trigger point (start-of-turn, resolved in `end_turn`'s incoming-seat sequence, §1b) so the seam exists; the full trademark catalog is Subsystem 2.

**Test strategy:** `hero_power_deals_declared_damage`: define a Boss with `DealDamage{2}`, activate → assert `boss.damaged` delta with `amount==2` **and** `available_juice` reduced by `hero_power_cost`. `hero_power_rejects_when_cost_understated`: command claims cost 0 → rejected. `hero_power_rejects_when_unaffordable` (extend existing affordability test).

---

## 5. Boss-locked cards (a real "only Boss X may deck this" mechanic)

Today "boss-specific" is approximated by `CardClass::Boss` + a Boss's `signature_card_ids` (boss_definition.rs), but nothing *enforces at deck-build* that a boss-locked card can only be run by its Boss. Add a real lock:

**(a) Catalog:** add `#[serde(default)] pub boss_lock: Option<String>` to `DefineCardCmd`/`CardDefined` — `Some(boss_id)` means "only this Boss may deck this card." Invariant in `validate_card_fields`: if `boss_lock.is_some()`, `class` must be `CardClass::Boss` (a locked card is a Boss card).

**(b) Enforcement point — Outfit/deck-build.** The `outfit` bounded context (crates/domain/src/outfit.rs) validates a constructed deck. Add an invariant to its card-add/save command: a card with `boss_lock == Some(b)` may only be added to an Outfit whose Boss is `b`. This is the true mechanic ("only boss X can deck this"), enforced where decks are assembled, not just implied by class.

**(c) Engine cross-check.** `game-session`'s `start_match` re-validates (server-authoritative, since decks back tradeable assets): every `CardInstance` with `boss_lock == Some(b)` in a seat's deck must have that seat's `boss_id == b`, else reject `StartMatchCmd` with an `InvariantViolation`. Belt-and-suspenders because the engine is the anti-cheat authority.

**Test strategy:** `outfit_rejects_boss_locked_card_for_wrong_boss`; `outfit_accepts_boss_locked_card_for_matching_boss`; `start_match_rejects_mismatched_boss_lock`; `define_card_rejects_boss_lock_on_non_boss_class`.

---

## 6. Command-drift fix (`AttackCmd` vs `DeclareAttackCmd`) + real WASM name-gate

### The drift
- The **client** action enum uses `kind: 'AttackCmd'` (model.ts:207, rules.ts:160/229) with fields `{ seat, attackerId, targetRef }`.
- The **Rust** aggregate recognizes `DeclareAttackCmd` (lib.rs:90) with fields `{ matchId, playerId, attackerId, defenderId }`.
- The transport `connection.ts:68` sends **only `action.kind`** as a bare text frame — no matchId, no payload — even though the server's `ClientMessage` envelope (`ws/mod.rs`) and `MatchHub::apply_action` (hub.rs:271) already accept a structured `{ command, payload }`. So an online attack would arrive as the bare string `"AttackCmd"`, which the aggregate does not recognize (`UnknownCommand`) — **online attacks are broken.**

### The fix (rename + real envelope; no silent translation shim)
1. **Rename to one canonical name.** Standardize on **`AttackCmd`** everywhere (shorter, already the client's word) — or keep `DeclareAttackCmd` and change the client; pick one and apply it in all three places: the Rust command const (lib.rs:90 `DECLARE_ATTACK`), the client `MatchAction` (model.ts:207), and the `targetRef`/`defenderId` field naming. **Recommendation: `AttackCmd`**, with the payload field named `targetRef` (the client's scheme: `boss:<seat>` | `op:<instanceId>`), because §2.2 combat targets units, not just the Boss — `defenderId` was a stub-era name for "the enemy Boss."
2. **Send the real envelope.** Change `connection.ts` `send()` to ship the structured frame the server already parses: `{ type: "action", matchId, command: action.kind, payload: <action fields> }` (JSON), not the bare `action.kind`. This activates the `payload`-carrying path that `hub.rs`/`ws/mod.rs` already implement.
3. **Make the WASM name-gate real.** `wasm_bindings::execute_command` (lib.rs:1798) today builds a **fresh empty** session and runs a **payload-less** bare-named command — it can never exercise real rules. Replace it with a stateful binding that mirrors the server:
   - Expose an opaque handle: `WasmGameSession` wrapping a `GameSession`, with `new(match_id) -> WasmGameSession`, `start(cmd_json) -> Result<JsValue, JsValue>` (returns emitted deltas as JSON), and `execute(command_name, payload_json) -> Result<JsValue, JsValue>` that builds `Command::with_payload(name, payload)` and calls `self.0.execute(...)`, returning the deltas (for optimistic prediction) or the domain-error text (for rejection) — **the identical decision the server's `apply_action` makes for the same input.**
   - This is what lets the client *predict* with the same code the server *authorizes* (companion spec §4.1). The old signature is replaced, not extended.

### Test strategy
- Rust: `attack_cmd_is_recognized` (the renamed command routes to combat, not `UnknownCommand`); a full combat command→event test (§2.2).
- WASM: a `wasm-bindgen-test` that starts a match and plays a card through `WasmGameSession`, asserting the returned deltas match a native `GameSession` run of the same commands (prediction == authority).
- Client: a `connection.ts` unit test asserting `send()` emits the structured JSON envelope (matchId + command + payload), not the bare kind.

---

## 7. Location-modifier hook (the City-pillar seam)

The City-as-map pillar (companion §3.1) must not force an engine rewrite later. Land the **seam** now, even though venue content and the map arrive in Subsystem 3.

**(a) Data-driven location on the match.** Add an optional `location: Option<LocationModifier>` to `StartMatchCmd`/`GameSession`:
```rust
pub struct LocationModifier {
    pub location_id: String,
    pub location_type: String,           // "bank" | "chop_shop" | "server_farm" | ... (data-driven)
    pub class_boosts: Vec<(CardClass, i8)>, // neutral, applies to BOTH seats (e.g. Hacker +1 atk at a server farm)
    pub heat_multiplier: u8,             // e.g. bank intensifies Heat/Cop response (default 1)
    pub event_table_ref: Option<String>, // seeded random-event table id (content fills later)
}
```
Default `None` = today's plain board (no behavior change; keeps every existing test green).

**(b) One application point.** Introduce a single private method `fn apply_location_modifiers(&self, seat: Player, base: &BoardUnit) -> BoardUnit` (and a Heat hook) that the engine consults where stats/Heat are read in combat and effect resolution. In Subsystem 1 it is the **identity function** when `location` is `None` and applies `class_boosts`/`heat_multiplier` when present — so the *hook* is exercised and tested, but the *content* (venue catalog, event tables) is Subsystem 3. Because modifiers are neutral (applied to both seats) and data-driven, growing from one district to the nested world (companion §3.1c) is adding location rows, not re-plumbing the engine.

**(c) Random-event seam.** Add a command stub `ResolveVenueEventCmd { match_id, event_table_ref, rng_draw }` that, like `ResolveCopEventCmd` (lib.rs:1598), draws from a seeded table (`ws/rng.rs` already exists) and emits a `venue.event.resolved` delta. In Subsystem 1 the table is a single no-op entry (emits the event, changes nothing) so the seam and its wiring ship and are tested; the outcome catalog (one-off swings + shared persistent boons → relics/idols) is Subsystem 3/5.

**Test strategy:** `location_none_is_identity` (existing combat tests unchanged with `location: None`); `location_boosts_matching_class_for_both_seats`; `resolve_venue_event_emits_delta_and_is_seeded`.

---

## 8. Build order within Subsystem 1 (suggested commit sequence)

Each step is independently testable (command→event) and keeps the workspace green:
1. **Juice crystal fix** (§1) — smallest, highest-value, pure defect fix. Ship first with its regression test.
2. **Card play-stats + type reconciliation** (§3.1–3.2) — catalog schema, no engine behavior yet.
3. **Semantic keywords + resolvable effects** (§3.3–3.4) — enums + mappings, unit-tested in isolation.
4. **Seat/board state + combat + effect resolution** (§2) — the big port; depends on 2–3.
5. **Hero powers with real effects** (§4) — depends on the effect machinery from 3–4.
6. **Boss-locked cards** (§5) — catalog + outfit + engine cross-check.
7. **Command-drift fix + real WASM gate** (§6) — depends on combat (§2) existing.
8. **Location-modifier seam** (§7) — additive, `None`-default, last so it wraps a complete engine.

## 9. Definition of done for Subsystem 1
- `crates/game-session` is the **authoritative, complete** engine: real seats/board/combat/effects/keywords/hero-powers; no boss-instakill stub; Juice ramps correctly and `play_card` mutates state.
- The client runs the **same** rules via a real, stateful WASM binding; a `wasm-bindgen-test` proves prediction == native authority for a representative command sequence.
- `web/src/match/rules.ts` no longer owns mutation logic (its rules are the WASM engine); `connection.ts` sends the structured envelope; the `AttackCmd`/`DeclareAttackCmd` drift is gone.
- The location seam exists and is `None`-default (no regression), so Subsystem 3 adds City content without re-plumbing.
- `make build` and `make test` are green; every new mechanic has a DDD command→event test; every bug fix (Juice pin-at-1, play-card no-deduct) has a dedicated regression test.
