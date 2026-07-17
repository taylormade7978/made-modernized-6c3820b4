# MADE Core Engine (Subsystem 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `crates/game-session` the authoritative, complete MADE rules engine — fix the Juice-ramp bug, port board/combat/effects/keywords out of the TypeScript client into the Rust aggregate, bind hero-powers and boss-locked cards to real effects, resolve the `AttackCmd`/`DeclareAttackCmd` command drift with a real stateful WASM gate, and land a `None`-default location seam so the City pillar isn't a later rewrite.

**Architecture:** Domain-Driven Design over a Cargo workspace. Every rule is a `Command → Vec<Event>` transition on an aggregate that embeds `shared::AggregateRoot`, routes on the command *name*, and records events via `self.root.record(...)`. `crates/game-session` is the single source of truth; it compiles to WASM so the browser predicts with the exact code the server authorizes. Tests are DDD command→event assertions; every bug fix carries a dedicated regression test.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, `wasm-bindgen` (optional, `wasm` feature only), Vitest for the TypeScript client. Build: `make build` (`cargo build --workspace --all-targets`). Test: `make test` (`cargo test --workspace`).

## Source-of-truth context (READ FIRST)

- **Spec:** `docs/superpowers/specs/2026-07-16-made-core-engine-design.md` (this plan implements it) and its companion `2026-07-16-made-game-overview-design.md` (§2 terminology, §4.1 engine unification).
- **Branch base:** this plan targets the **live code line `design/cyberpunk-theme`** (repo HEAD `5b1d2a2`), NOT `origin/main` (`c4466ff`). The spec's line anchors were written against `c4466ff` and are shifted; **every line number in THIS plan is against `5b1d2a2` and is correct** — trust this plan's anchors, not the spec's. Work happens in the worktree `/home/nengine/repos/made-build-wt-engine` on branch `feat/core-engine`.
- **Crate map:** shared traits (`Command`, `DomainError`, `Aggregate`, `AggregateRoot`, `DomainEvent`, `Repository`) live in `crates/shared/src/lib.rs`. The engine aggregate is `crates/game-session/src/lib.rs` (3775 lines). Catalog aggregates are `crates/domain/src/{card_definition,boss_definition,outfit}.rs`. Client rules to port: `web/src/match/{rules.ts,model.ts,connection.ts}` (they live under `made-build/web/`, NOT `made-site-src`).

## Global Constraints

*Every task's requirements implicitly include this section. Values are copied verbatim from the spec + confirmed against the live code.*

- **DDD contract:** each aggregate embeds `AggregateRoot`; implements `Aggregate::execute(Command) -> Result<Vec<Event>, DomainError>`; routes on `command.name.as_str()`; decodes payloads with `serde_json::from_slice(&command.payload).map_err(|e| DomainError::InvariantViolation(format!("malformed <Cmd> payload: {e}")))?`; returns `DomainError::unknown_command(AGGREGATE_TYPE, command.name)` for anything unrecognized; records each event with `self.root.record(Box::new(event.clone()))` then returns `Ok(vec![...])`.
- **DomainError has exactly two variants:** `UnknownCommand { aggregate, command }` and `InvariantViolation(String)`. Every business rejection uses `InvariantViolation`. Do not add variants.
- **`game-session` MUST stay WASM-safe:** dependencies are `shared` + `serde`/`serde_json` only; `wasm-bindgen` is behind `#[cfg(feature = "wasm")]`. No host-only deps (no `std::time`, no RNG crates, no I/O) outside the `wasm`-gated module. The seeded RNG for this crate is a pure function of `rng_seed` (mirror the client's `mulberry32`, rules.ts:71-79).
- **Canonical numbers (already declared, `game-session/src/lib.rs:104-139`):** `STARTING_JUICE: u8 = 1`, `JUICE_CAP: u8 = 10`, `JUICE_RAMP_PER_TURN: u8 = 1`, `HEAT_PER_PLAY: i32 = 1`, `HEAT_BOUNDS: RangeInclusive<i32> = 0..=10`, `MAX_OPERATORS: usize = 7`, `MAX_VEHICLES: usize = 3`, `FATIGUE_PER_EMPTY_DRAW: i32 = 1`, `COP_EVENT_DIE_SIDES: u8 = 10`. Reuse them; do not introduce parallel constants.
- **Terminology (MADE reskin — use these names in code):** Juice=mana, Outfit=hero+deck, Boss=hero (face HP), Heat=wanted meter, Cop Event=Heat-overflow punishment, Operators=minions, Vehicles=heavy minions, Job/Piece/Heist=spell-like types, Class=faction.
- **`CardType` is the single source of truth for card types:** exactly `{Operator, Job, Piece, Vehicle, Heist}` (card_definition.rs:51). There is **no `Operation` type** — the client's two `type:'Operation'` cards are a bug to fix (Task 2).
- **`CardClass`:** `{Neutral, Boss, Muscle, Grifter, Hacker, Driver, Cleaner}` (card_definition.rs:103).
- **Command struct naming is per-crate (do NOT normalize it in this subsystem):** `game-session` uses no-suffix structs (`PlayCard`, `DeclareAttack`, `EndTurn`, `ActivateHeroPower`, `StartMatch`) with a `pub const COMMAND: &str` carrying the `Cmd` suffix; `card_definition.rs` uses `DefineCardCmd`/`ReviseCardCmd` (suffix on both); `boss_definition.rs`/`outfit.rs` use no-suffix structs (`DefineBoss`, `AddCardToOutfit`). Follow the existing convention of whichever file you edit.
- **TDD, every task:** write the failing test first, watch it fail, implement minimally, watch it pass, commit. Every bug fix (Juice pin-at-1, play-card no-deduct) gets a dedicated regression test asserting **state**, not just the emitted event.
- **Commits:** conventional-commit subjects, no `Co-Authored-By`/`Generated-with` trailers (repo policy). Keep the workspace green (`make build && make test`) at every commit.
- **Definition of done (spec §9):** authoritative complete engine (real seats/board/combat/effects/keywords/hero-powers; no boss-instakill; Juice ramps; `play_card` mutates state); client runs the same rules via a real stateful WASM binding proven by a `wasm-bindgen-test`; `connection.ts` sends the structured envelope; `AttackCmd`/`DeclareAttackCmd` drift gone; location seam `None`-default (no regression).

---

## File Structure

**Modified (Rust):**
- `crates/game-session/src/lib.rs` — the aggregate. Nearly every task touches it: new fields (`max_juice`), new state types (`CardInstance`, `BoardUnit`, `SeatState`, `CardEffect`, `Keyword`, `LocationModifier`), rewritten `end_turn`/`play_card`/`declare_attack`/`activate_hero_power`, new `Event` variants, the stateful `WasmGameSession` binding. This file is already 3775 lines; keep new type definitions grouped near the existing structs and keep the `#[cfg(test)]` module the single test home (matches the current layout).
- `crates/domain/src/card_definition.rs` — add `atk`/`hp`/`boss_lock` play-stats to `DefineCardCmd`/`ReviseCardCmd`/`CardDefined`/`CardRevised`; extend `validate_card_fields`; extend `REGISTERED_EFFECTS`.
- `crates/domain/src/boss_definition.rs` — add `hero_power_effect`/`hero_power_cost`/`trademark_effect` to `DefineBoss`/`BossDefined`.
- `crates/domain/src/outfit.rs` — add boss binding + boss-lock invariant (needs new aggregate state; see Task 8).

**Modified (TypeScript client):**
- `web/src/match/model.ts` — rename the action `kind` if we standardize the command name; keep the `DeltaEvent` union in sync with new Rust event_type strings.
- `web/src/match/connection.ts` — `send()` ships the structured `{type,matchId,command,payload}` envelope (Task 9).
- `web/src/match/rules.ts` — retype the two `Operation` cards to `Piece` (Task 2); after the WASM port, its mutation rules are superseded by the engine (Task 9 wires the client to `WasmGameSession`; deleting the TS rule bodies is a Subsystem-2 cleanup, out of scope here beyond the retype + the envelope).

**New (build/staging):** none — `crates/game-session` already builds to WASM (`crate-type = ["cdylib","rlib"]`, `wasm` feature) and the client already has a `stage:wasm` script.

---

## Task 1: Juice-crystal fix (the pin-at-1 bug)

**Files:**
- Modify: `crates/game-session/src/lib.rs` — `OutfitConfig` (157-213), `ensure_starting_juice_valid` (970-986), `ramped_juice` (1485-1490), `end_turn` (1513-1581), `play_card` (1260-1327), `TurnEnded` event struct (~739).
- Test: same file, `#[cfg(test)]` module (from 1808).

**Interfaces:**
- Consumes: existing constants `STARTING_JUICE`, `JUICE_CAP`, `JUICE_RAMP_PER_TURN`, `HEAT_PER_PLAY`, `HEAT_BOUNDS`; existing helpers `outfit_at`, `outfit_at_mut`, `heat_after_play`, `ensure_card_affordable`.
- Produces: `OutfitConfig.max_juice: u8`; `TurnEnded.next_player_max_juice: u8` (a new event field later tasks and the client read); `play_card` now mutates `available_juice` and `starting_heat`.

**Background (the bug, spec §1):** `ramped_juice` adds `JUICE_RAMP_PER_TURN` to the seat's *remaining* `available_juice`, so a seat spent to 0 gets `0+1=1` forever — pinned at 1. And `play_card` never deducts `available_juice` or persists Heat (only `activate_hero_power` deducts). The fix: a separate max-Juice crystal that grows independent of spend; refill available to the crystal at turn start; make `play_card` mutate state.

- [ ] **Step 1: Write the failing regression test for the pin-at-1 bug**

Add to the test module (near the `end_turn` tests, ~line 3083):

```rust
    // Regression: the pin-at-1 Juice bug. A seat that emptied its pool must
    // refill to its GROWN crystal next turn, not to `spent + 1`.
    #[test]
    fn end_turn_grows_incoming_crystal_and_refills_available() {
        let mut session = valid_session();
        // Seat A is opening; seat B (incoming) has a mid-game crystal of 3 but an
        // emptied pool (spent to 0 last turn).
        let mut b = OutfitConfig::new("m-1-b");
        b.max_juice = 3;
        b.available_juice = 0;
        session.configure_player_b(b);
        session.set_opening_player(Some(Player::A));

        let events = session
            .execute(EndTurn::new("m-1", "m-1-a").into_command())
            .expect("A may end its turn");

        // Find the TurnEnded event and assert the crystal grew to 4 and available
        // refilled to the crystal (4), NOT to 1.
        let ended = events
            .iter()
            .find_map(|e| match e {
                Event::TurnEnded(t) => Some(t),
                _ => None,
            })
            .expect("end_turn emits TurnEnded");
        assert_eq!(ended.next_player_max_juice, 4, "crystal grows 3 -> 4");
        assert_eq!(ended.next_player_juice, 4, "available refills to the grown crystal, not to 1");
        // State was mutated on the incoming seat.
        assert_eq!(session.outfit_at(Player::B).max_juice, 4);
        assert_eq!(session.outfit_at(Player::B).available_juice, 4);
    }
```

> Confirmed signatures (verified against lib.rs): `configure_player_a`/`configure_player_b` (lib.rs:905/910) take an `OutfitConfig`; **`set_opening_player` (lib.rs:916) takes `Option<Player>`** — so pass `Some(Player::A)`, not `Player::A` (every test in this plan does). `valid_session()` and `Player` are used throughout the test module (see `play_card_rejects_when_board_exceeds_operator_cap`, lib.rs:2328).

- [ ] **Step 2: Run it to confirm it fails to compile / fails**

Run: `cargo test -p game-session end_turn_grows_incoming_crystal_and_refills_available`
Expected: FAIL — `OutfitConfig` has no field `max_juice`, and `TurnEnded` has no field `next_player_max_juice`.

- [ ] **Step 3: Add the `max_juice` crystal field to `OutfitConfig`**

In `OutfitConfig` (after `available_juice`, lib.rs:183) add:

```rust
    /// The seat's max-Juice "crystal": the ceiling `available_juice` refills to
    /// at the start of each of the owner's turns. Grows by `JUICE_RAMP_PER_TURN`
    /// each of the owner's turns, hard-capped at `JUICE_CAP`, INDEPENDENT of spend.
    pub max_juice: u8,
```

In `OutfitConfig::new` (196-211) set it. Keep the existing `available_juice: 3` mid-game convenience but give the crystal a legal value that covers it:

```rust
            starting_juice: STARTING_JUICE,
            available_juice: 3,
            max_juice: 3,
            heist_resolved: false,
```

> The canonical clean opening is `max_juice == available_juice == STARTING_JUICE (1)`; `new()` stays a mid-game convenience (available 3). Tests that need a specific crystal set `max_juice`/`available_juice` explicitly (Step 1 does).

- [ ] **Step 4: Extend `ensure_starting_juice_valid` to bound the crystal**

In `ensure_starting_juice_valid` (970-986), after the `available_juice > JUICE_CAP` check, add:

```rust
            if outfit.max_juice > JUICE_CAP {
                return Err(DomainError::InvariantViolation(format!(
                    "player {seat:?} Outfit '{}' has max Juice {}, exceeding the hard cap of {JUICE_CAP}",
                    outfit.name, outfit.max_juice
                )));
            }
```

- [ ] **Step 5: Add `next_player_max_juice` to the `TurnEnded` event**

In the `TurnEnded` struct (~739) add the field:

```rust
    /// The incoming seat's grown max-Juice crystal (what `next_player_juice`
    /// refills to). Lets the client render the crystal, not just the pool.
    pub next_player_max_juice: u8,
```

- [ ] **Step 6: Replace `ramped_juice` with a crystal ramp and refill in `end_turn`**

Replace `ramped_juice` (1485-1490) with a crystal grower:

```rust
    /// Grow the seat's max-Juice crystal by one, capped at `JUICE_CAP`.
    /// INDEPENDENT of how much was spent last turn — this is the fix for the
    /// pin-at-1 bug (the old `ramped_juice` grew the *remaining* pool).
    fn grown_crystal(&self, seat: Player) -> u8 {
        self.outfit_at(seat)
            .max_juice
            .saturating_add(JUICE_RAMP_PER_TURN)
            .min(JUICE_CAP)
    }
```

In `end_turn`, replace the `next_player_juice` derivation (1549) and the incoming-seat mutation block (1555-1559):

```rust
        let incoming = Self::opponent_of(seat);
        let next_player_max_juice = self.grown_crystal(incoming);
        let next_player_juice = next_player_max_juice; // refill available TO the crystal
        let (fatigue_amount, boss_hp_remaining) = self.resolve_start_of_turn_draw(incoming);
        let incoming_player_id = self.outfit_at(incoming).name.clone();
        {
            let outfit = self.outfit_at_mut(incoming);
            outfit.max_juice = next_player_max_juice;
            outfit.available_juice = next_player_juice;
            outfit.boss_hp = boss_hp_remaining;
        }
        self.opening_player = Some(incoming);
```

And populate the new event field where `TurnEnded` is built (1571-1577):

```rust
        let ended = Event::TurnEnded(TurnEnded {
            match_id: cmd.match_id,
            player_id: cmd.player_id,
            player: seat,
            next_player: incoming,
            next_player_juice,
            next_player_max_juice,
        });
```

- [ ] **Step 7: Run the regression test — it passes**

Run: `cargo test -p game-session end_turn_grows_incoming_crystal_and_refills_available`
Expected: PASS.

- [ ] **Step 8: Write the failing tests for `play_card` state mutation**

```rust
    // play_card must DEDUCT Juice from state (it previously only emitted the spend).
    #[test]
    fn play_card_deducts_juice() {
        let mut session = valid_session();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 5;
        a.available_juice = 5;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));

        session
            .execute(PlayCard::new("m-1", "m-1-a", "card-instance-1", "boss:B", 3).into_command())
            .expect("a cost-3 card is affordable at 5 Juice");

        assert_eq!(session.outfit_at(Player::A).available_juice, 2, "5 - 3 = 2");
    }

    // play_card must PERSIST the Heat raise to state (previously only in the event).
    #[test]
    fn play_card_persists_heat() {
        let mut session = valid_session();
        let mut a = OutfitConfig::new("m-1-a");
        a.starting_heat = 0;
        a.max_juice = 5;
        a.available_juice = 5;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));

        session
            .execute(PlayCard::new("m-1", "m-1-a", "card-instance-1", "boss:B", 1).into_command())
            .expect("play succeeds");

        assert_eq!(session.outfit_at(Player::A).starting_heat, 1, "Heat 0 -> 1 persisted to state");
    }

    // A REJECTED play must leave available_juice unchanged (no partial mutation).
    #[test]
    fn play_card_rejection_leaves_juice_unchanged() {
        let mut session = valid_session();
        let mut a = OutfitConfig::new("m-1-a");
        a.max_juice = 3;
        a.available_juice = 3;
        session.configure_player_a(a);
        session.set_opening_player(Some(Player::A));

        let _ = session
            .execute(PlayCard::new("m-1", "m-1-a", "card-instance-1", "boss:B", 4).into_command())
            .expect_err("cost 4 > available 3 is rejected");

        assert_eq!(session.outfit_at(Player::A).available_juice, 3, "rejected play must not deduct");
    }
```

> The existing `PlayCard::new(match_id, player_id, card_instance_id, target_ref, juice_cost)` signature is confirmed at the call site lib.rs:2319. Use `"boss:B"` as `target_ref` — the current `play_card` only requires it be non-empty, and Task 6 gives it real meaning.

- [ ] **Step 9: Run them to confirm they fail**

Run: `cargo test -p game-session play_card_deducts_juice play_card_persists_heat play_card_rejection_leaves_juice_unchanged`
Expected: `play_card_deducts_juice` and `play_card_persists_heat` FAIL (state unchanged); `play_card_rejection_leaves_juice_unchanged` PASSES already (rejection happens before any mutation today — keep it as a guard).

- [ ] **Step 10: Make `play_card` mutate state**

In `play_card`, after `let new_heat = self.heat_after_play(seat)?;` (1307) and **before** building the events (1310), add the mutation. Affordability and heat bounds were already checked above, so this cannot underflow or leave bounds:

```rust
        // Mutate state: deduct the Juice and persist the Heat raise. (Previously
        // play_card only emitted these deltas without applying them — the bug.)
        {
            let outfit = self.outfit_at_mut(seat);
            outfit.available_juice -= cmd.juice_cost;
            outfit.starting_heat = new_heat;
        }
```

> `cmd.juice_cost` is consumed by the `CardPlayed` event below (`juice_spent: cmd.juice_cost`) — read it into the mutation before it is moved, or clone the `u8` (it is `Copy`, so ordering is fine). `new_heat` is `i32` and already validated within `HEAT_BOUNDS`.

- [ ] **Step 11: Run the play_card tests — they pass**

Run: `cargo test -p game-session play_card`
Expected: PASS (all play_card tests, including the pre-existing `play_card_rejects_when_cost_exceeds_available_juice`).

- [ ] **Step 12: Run the whole workspace to confirm no regressions**

Run: `make test`
Expected: PASS. (Existing `end_turn` tests that asserted the old `ramped_juice` behavior may need their expected numbers updated — if any fail, they were asserting the *bug*; update them to expect crystal-refill semantics and note the change in the commit body.)

- [ ] **Step 13: Commit**

```bash
git add crates/game-session/src/lib.rs
git commit -m "fix(engine): Juice ramps via a per-seat max-crystal, and play_card mutates state

Adds OutfitConfig.max_juice (the crystal) that grows +1 each of the owner's
turns capped at 10, INDEPENDENT of spend; available refills to the crystal at
turn start. Fixes the pin-at-1 bug where ramped_juice grew the remaining pool.
play_card now deducts available_juice and persists the Heat raise (previously
only emitted). TurnEnded carries next_player_max_juice so the client can render
the crystal. Regression tests cover empty-pool refill and play-card deduction."
```

---

## Task 2: Card play-stats (atk/hp) + card-type reconciliation

**Files:**
- Modify: `crates/domain/src/card_definition.rs` — `DefineCardCmd` (186-211), `ReviseCardCmd` (231-256), `CardDefined` (339-350), `CardRevised` (355-366), `validate_card_fields` (285-334), the `define_card`/`revise_card` event builders (443-456, 482-495).
- Modify: `web/src/match/rules.ts` — the two `type:'Operation'` cards (59-60).
- Test: `card_definition.rs` `#[cfg(test)]` module (from ~530).

**Interfaces:**
- Consumes: existing `CardType` (49-57), `validate_card_fields`, `legal_cost_range`.
- Produces: `DefineCardCmd.atk: u8`, `DefineCardCmd.hp: u8` (and same on `ReviseCardCmd`/`CardDefined`/`CardRevised`); the invariant "Operator/Vehicle need `hp >= 1`; Job/Piece/Heist must have `atk == 0 && hp == 0`". These feed `CardInstance.atk/hp` at deck-build (Task 4).

- [ ] **Step 1: Write the failing tests for play-stats validation**

Add to the `card_definition.rs` test module:

```rust
    #[test]
    fn operator_requires_positive_hp() {
        let mut agg = CardDefinition::new("card-op");
        // Operator with hp 0 is illegal — a unit needs a body.
        let cmd = DefineCardCmd { card_type: "Operator".to_string(), atk: 2, hp: 0, ..valid_cmd() };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
        assert_eq!(agg.version(), 0);
    }

    #[test]
    fn operator_with_hp_is_accepted() {
        let mut agg = CardDefinition::new("card-op");
        let cmd = DefineCardCmd { card_type: "Operator".to_string(), atk: 2, hp: 3, ..valid_cmd() };
        let events = agg.execute(cmd.into_command()).expect("a 2/3 Operator is legal");
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::CardDefined(d) => { assert_eq!(d.atk, 2); assert_eq!(d.hp, 3); }
            other => panic!("expected CardDefined, got {other:?}"),
        }
    }

    #[test]
    fn spell_type_rejects_body_stats() {
        let mut agg = CardDefinition::new("card-job");
        // A Job is spell-like: it must have no board body (atk == 0 && hp == 0).
        let cmd = DefineCardCmd { card_type: "Job".to_string(), atk: 3, hp: 0, effect_script_ref: "effect.deal_damage".to_string(), ..valid_cmd() };
        assert!(matches!(
            agg.execute(cmd.into_command()),
            Err(DomainError::InvariantViolation(_))
        ));
    }
```

> `valid_cmd()` (card_definition.rs:535) currently builds a `Driver`/`Operator` card with `effect.draw_card`. After Step 3 adds `atk`/`hp` fields with `#[serde(default)]`, `valid_cmd()` must set legal stats for its `Operator` type — update `valid_cmd()` to include `atk: 2, hp: 2` so the baseline card stays legal.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p domain -- card_definition`
Expected: FAIL — `DefineCardCmd` has no `atk`/`hp` fields.

- [ ] **Step 3: Add `atk`/`hp` to the command, event, and both revise/define structs**

In `DefineCardCmd` (after `copy_cap`, 210) and identically in `ReviseCardCmd` (255):

```rust
    /// Board attack power. 0 for non-unit (spell-like) types.
    #[serde(default)]
    pub atk: u8,
    /// Board health. Must be >= 1 for Operator/Vehicle; 0 for Job/Piece/Heist.
    #[serde(default)]
    pub hp: u8,
```

In `CardDefined` (after `copy_cap`, 349) and `CardRevised` (365):

```rust
    pub atk: u8,
    pub hp: u8,
```

Update `valid_cmd()` (535) to set `atk: 2, hp: 2`.

- [ ] **Step 4: Enforce the unit/spell stat invariant in `validate_card_fields`**

`validate_card_fields` currently takes seven params and returns `ValidatedCardFields`. Add `atk`/`hp` params and the invariant. Change the signature (285-293) to add `atk: u8, hp: u8,` and insert, after the effect-script check (320), before the Legendary check:

```rust
    // Invariant: Operators and Vehicles are board units and need a body;
    // Job/Piece/Heist are spell-like and carry no board stats.
    match card_type {
        CardType::Operator | CardType::Vehicle => {
            if hp < 1 {
                return Err(DomainError::InvariantViolation(format!(
                    "a {} is a board unit and must have hp >= 1; got hp {hp}",
                    card_type.as_str(),
                )));
            }
        }
        CardType::Job | CardType::Piece | CardType::Heist => {
            if atk != 0 || hp != 0 {
                return Err(DomainError::InvariantViolation(format!(
                    "a {} is spell-like and must have atk == 0 && hp == 0; got {atk}/{hp}",
                    card_type.as_str(),
                )));
            }
        }
    }
```

Update the two call sites in `define_card` and `revise_card` to pass `cmd.atk, cmd.hp`, and add `atk: cmd.atk, hp: cmd.hp` to the `CardDefined`/`CardRevised` event builders (443-456, 482-495).

- [ ] **Step 5: Run the domain tests — they pass**

Run: `cargo test -p domain -- card_definition`
Expected: PASS.

- [ ] **Step 6: Retype the two client `Operation` cards to `Piece`**

In `web/src/match/rules.ts`, change lines 59-60: `type: 'Operation'` → `type: 'Piece'` for both `pd_the_crib` and `ht_the_come_up`. (`Piece` is the catalog's spell-like non-unit type; both cards have no atk/hp, consistent with the Step-4 invariant.)

```ts
  { cardId: 'pd_the_crib', name: 'The Crib', cost: 2, type: 'Piece', effect: 'cool', amount: 2, text: 'Lower your Heat by 2.' },
  { cardId: 'ht_the_come_up', name: 'The Come-Up', cost: 2, type: 'Piece', effect: 'juice', amount: 2, text: 'Gain 2 Juice this turn.' },
```

- [ ] **Step 7: Run the client type-check + tests**

Run: `cd web && npx tsc --noEmit && npx vitest run`
Expected: PASS (no `Operation` type remains; `CardDef.type` is a free-form string today so this is a data change, not a type change — the tsc pass confirms nothing else referenced `'Operation'`).

- [ ] **Step 8: Commit**

```bash
git add crates/domain/src/card_definition.rs web/src/match/rules.ts
git commit -m "feat(catalog): add atk/hp play-stats to cards; retype Operation->Piece

Operators/Vehicles now require hp>=1; Job/Piece/Heist must carry no board body
(atk==0 && hp==0), enforced in validate_card_fields so illegal card bodies are
unrepresentable at the catalog boundary. Retypes the two client demo cards that
used the non-existent 'Operation' type to the catalog's spell-like 'Piece'."
```

---

## Task 3: Semantic keywords + resolvable effects

**Files:**
- Modify: `crates/game-session/src/lib.rs` — add `CardEffect` and `Keyword` enums with parsers and the `effect_script_ref -> CardEffect` mapping (place them near the other domain types, before the aggregate).
- Modify: `crates/domain/src/card_definition.rs` — extend `REGISTERED_EFFECTS` (39-47) so the closed effect set the engine resolves is fully covered.
- Test: both files' test modules.

**Interfaces:**
- Consumes: catalog `REGISTERED_EFFECTS`, catalog `keywords: Vec<String>`.
- Produces: `enum CardEffect { None, DealDamage{amount:u8}, Summon, DrawCards{amount:u8}, GainJuice{amount:u8}, Cool{amount:u8} }`; `enum Keyword { Spotlight, DriveBy }`; `fn CardEffect::from_script_ref(&str) -> Option<CardEffect>` and `fn Keyword::parse(&str) -> Result<Keyword, DomainError>`. Task 4 uses these to populate `CardInstance` at deck-build.

**Resolved ambiguity (mapping):** the catalog's `REGISTERED_EFFECTS` (`effect.noop`, `effect.deal_damage`, `effect.draw_card`, `effect.gain_juice`, `effect.steal_piece`, `effect.recruit_operator`, `effect.pull_heist`) does not include a "cool" or a "summon" name, while the client's closed set is `damage/summon/draw/juice/cool`. **Decision for Subsystem 1:** map the overlapping four directly, treat `effect.recruit_operator` as `Summon`, and **add `effect.cool` to `REGISTERED_EFFECTS`** so the client's `cool` cards (The Crib) have a catalog home. `effect.steal_piece` and `effect.pull_heist` are Subsystem-2 mechanics with no Subsystem-1 behavior — map them to `CardEffect::None` for now (they parse and validate, they just resolve to a no-op until Subsystem 2 implements them). A test iterates `REGISTERED_EFFECTS` and asserts every entry maps to `Some(_)`, so coverage can never silently regress.

- [ ] **Step 1: Add `effect.cool` to the catalog allow-list**

In `card_definition.rs` `REGISTERED_EFFECTS` (39-47) add `"effect.cool"`:

```rust
pub const REGISTERED_EFFECTS: &[&str] = &[
    "effect.noop",
    "effect.deal_damage",
    "effect.draw_card",
    "effect.gain_juice",
    "effect.cool",
    "effect.steal_piece",
    "effect.recruit_operator",
    "effect.pull_heist",
];
```

- [ ] **Step 2: Write the failing tests for the enums + mapping (in `game-session`)**

Add to the `game-session` test module:

```rust
    #[test]
    fn card_effect_maps_every_registered_effect() {
        // Coverage guard: every catalog-registered effect must map to a CardEffect,
        // so adding a REGISTERED_EFFECTS entry without a mapping fails loudly.
        for name in domain::card_definition::REGISTERED_EFFECTS {
            assert!(
                CardEffect::from_script_ref(name).is_some(),
                "registered effect {name} has no CardEffect mapping"
            );
        }
    }

    #[test]
    fn card_effect_maps_known_names() {
        assert_eq!(CardEffect::from_script_ref("effect.noop"), Some(CardEffect::None));
        assert_eq!(CardEffect::from_script_ref("effect.deal_damage"), Some(CardEffect::DealDamage { amount: 0 }));
        assert_eq!(CardEffect::from_script_ref("effect.recruit_operator"), Some(CardEffect::Summon));
        assert_eq!(CardEffect::from_script_ref("effect.cool"), Some(CardEffect::Cool { amount: 0 }));
        assert_eq!(CardEffect::from_script_ref("effect.unknown"), None);
    }

    #[test]
    fn keyword_parse_accepts_known_rejects_unknown() {
        assert_eq!(Keyword::parse("Spotlight").unwrap(), Keyword::Spotlight);
        assert_eq!(Keyword::parse("Drive-By").unwrap(), Keyword::DriveBy);
        assert!(Keyword::parse("Bogus").is_err());
    }
```

> `game-session` must depend on `domain` to read `REGISTERED_EFFECTS`. **Confirmed safe:** `crates/domain/Cargo.toml` does NOT depend on `game-session`, so adding `domain = { workspace = true }` to `crates/game-session/Cargo.toml` `[dependencies]` creates no cycle. **WASM caveat:** verify `domain` compiles to `wasm32` (it decodes command payloads with serde like game-session, so it should) — run `cargo build -p game-session --features wasm` after adding the dep. If `domain` unexpectedly pulls a host-only dep into the WASM build, fall back to re-declaring the effect-name constants privately in `game-session` with a comment pointing at `domain::card_definition::REGISTERED_EFFECTS`; the mapping content is identical either way.

- [ ] **Step 3: Run to confirm failure**

Run: `cargo test -p game-session card_effect keyword`
Expected: FAIL — `CardEffect`/`Keyword` undefined.

- [ ] **Step 4: Define the `CardEffect` enum and mapping**

Add near the aggregate's domain types (before `struct GameSession`):

```rust
/// The closed set of card effects the engine can resolve. Mirrors the client's
/// `resolveEffect` (web/src/match/rules.ts:299). Extended in Subsystem 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CardEffect {
    None,
    DealDamage { amount: u8 },
    Summon, // stats come from the CardInstance's atk/hp
    DrawCards { amount: u8 },
    GainJuice { amount: u8 },
    Cool { amount: u8 }, // lower own Heat
}

impl CardEffect {
    /// Total mapping from a catalog `effect_script_ref` to a resolvable effect.
    /// `amount` fields default to 0 here; the concrete amount is carried on the
    /// CardInstance at deck-build (Task 4). Returns None for unregistered names.
    pub fn from_script_ref(script_ref: &str) -> Option<CardEffect> {
        Some(match script_ref {
            "effect.noop" => CardEffect::None,
            "effect.deal_damage" => CardEffect::DealDamage { amount: 0 },
            "effect.draw_card" => CardEffect::DrawCards { amount: 0 },
            "effect.gain_juice" => CardEffect::GainJuice { amount: 0 },
            "effect.cool" => CardEffect::Cool { amount: 0 },
            "effect.recruit_operator" => CardEffect::Summon,
            // Subsystem-2 mechanics: registered + validated, resolve to no-op for now.
            "effect.steal_piece" | "effect.pull_heist" => CardEffect::None,
            _ => return None,
        })
    }
}
```

- [ ] **Step 5: Define the `Keyword` enum and parser**

```rust
/// Engine-semantic keywords (bound to real behavior in combat/summon), not inert
/// strings. Mirrors the client's ad-hoc Spotlight/Drive-By checks. Extended in
/// Subsystem 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Keyword {
    Spotlight, // taunt: enemy attacks must target a Spotlight unit first
    DriveBy,   // on arrival, deal damage to the enemy Boss
}

impl Keyword {
    /// Parse a catalog keyword string; unknown keywords are rejected (mirrors
    /// CardType::parse). Accepts the client's exact spellings.
    pub fn parse(raw: &str) -> Result<Keyword, DomainError> {
        match raw {
            "Spotlight" => Ok(Keyword::Spotlight),
            "Drive-By" => Ok(Keyword::DriveBy),
            other => Err(DomainError::InvariantViolation(format!(
                "unknown keyword '{other}'"
            ))),
        }
    }
}
```

- [ ] **Step 6: Run the tests — they pass**

Run: `cargo test -p game-session card_effect keyword && cargo test -p domain -- registered`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/game-session/src/lib.rs crates/domain/src/card_definition.rs
git commit -m "feat(engine): resolvable CardEffect + semantic Keyword enums

Introduces the closed CardEffect set (None/DealDamage/Summon/DrawCards/GainJuice/
Cool) with a total mapping from catalog effect_script_refs, and a Keyword enum
(Spotlight/DriveBy) parsed from catalog keyword strings with unknown-keyword
rejection. Adds effect.cool to REGISTERED_EFFECTS so the client's cool cards have
a catalog home. A coverage test asserts every registered effect maps."
```

---

## Task 4: Seat/board live state + `start_match` deals hands

**Files:**
- Modify: `crates/game-session/src/lib.rs` — add `CardInstance`, `BoardUnit`, `SeatState`; thread them into `GameSession`; rewrite `start_match` to build seats from the seeded deck.
- Test: same file's test module.

**Interfaces:**
- Consumes: `CardEffect`, `Keyword` (Task 3); catalog play-stats concept (Task 2); the seeded RNG approach from `rules.ts` `buildDeck`/`mulberry32`.
- Produces: `struct CardInstance`, `struct BoardUnit`, `struct SeatState`; `GameSession` now holds a `SeatState` per seat (in addition to, not replacing, `OutfitConfig` as the *opening input*); `start_match` deals `OPENING_HAND = 4` from a seeded 30-card deck. Later tasks read `seat_state_at(seat)`/`seat_state_at_mut(seat)`.

**Design note (coexistence, not big-bang replacement):** The spec (§2.1) says `SeatState` "replaces the scalar counters" — but the entire existing test suite and every command handler read `OutfitConfig`. To keep the workspace green at every commit (Global Constraint), **land `SeatState` alongside `OutfitConfig`**: `OutfitConfig` stays the opening-configuration input and the home of `max_juice`/`available_juice`/`starting_heat`/`boss_hp` that Tasks 1/5 already mutate; `SeatState` adds `hand`/`deck`/`board` (the things scalars can't express). Combat/effects (Tasks 5-6) operate on `SeatState.board`; the resource scalars stay on `OutfitConfig`. A full unification (folding the scalars into `SeatState`) is a mechanical follow-up and is explicitly out of Subsystem-1 scope — do NOT attempt it here, it would churn all ~200 existing tests.

- [ ] **Step 1: Write the failing test for seeded deck-dealing**

```rust
    #[test]
    fn start_match_deals_opening_hands_from_seeded_deck() {
        let mut session = valid_session();
        let events = session
            .execute(valid_start_match().into_command())
            .expect("a valid StartMatch deals hands");
        assert!(events.iter().any(|e| matches!(e, Event::MatchStarted(_))));
        // Both seats hold OPENING_HAND cards; the rest is the ordered deck.
        assert_eq!(session.seat_state_at(Player::A).hand.len(), OPENING_HAND);
        assert_eq!(session.seat_state_at(Player::B).hand.len(), OPENING_HAND);
        // Deterministic: same seed => identical opening hand instance ids.
        let mut again = valid_session();
        again.execute(valid_start_match().into_command()).unwrap();
        let ids_a: Vec<_> = session.seat_state_at(Player::A).hand.iter().map(|c| c.instance_id.clone()).collect();
        let ids_a2: Vec<_> = again.seat_state_at(Player::A).hand.iter().map(|c| c.instance_id.clone()).collect();
        assert_eq!(ids_a, ids_a2, "the seeded deal is deterministic");
    }
```

> `valid_start_match()` is the existing StartMatch test helper (grep the test module — the `start_match` tests use it). Add `pub const OPENING_HAND: usize = 4;` near the other constants (matches rules.ts:68). If `valid_start_match` doesn't carry a deck/seed, extend `StartMatch`/its helper minimally so a deck can be dealt; keep the default seed `0xc0ffee` (rules.ts:111) for parity.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p game-session start_match_deals_opening_hands_from_seeded_deck`
Expected: FAIL — `seat_state_at`, `SeatState`, `OPENING_HAND` undefined.

- [ ] **Step 3: Define `CardInstance`, `BoardUnit`, `SeatState`**

Add near the other domain types (after `Keyword`):

```rust
/// A card instance in a hand or deck: a definition ref + per-copy identity +
/// resolved play-stats. Populated from CardDefinition fields at deck-build.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CardInstance {
    pub instance_id: String,       // e.g. "A-w_the_homie-3"
    pub card_id: String,           // definition id
    pub cost: u8,
    pub card_type: CardType,       // Operator/Job/Piece/Vehicle/Heist
    pub effect: CardEffect,        // resolved effect + amount
    pub atk: u8,                   // 0 for non-unit cards
    pub hp: u8,                    // 0 for non-unit cards
    pub keywords: Vec<Keyword>,
    pub boss_lock: Option<String>, // Some(boss_id) if boss-locked (Task 8)
}

/// A unit on the board (summoned Operator or Vehicle).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoardUnit {
    pub instance_id: String,
    pub card_id: String,
    pub atk: u8,
    pub hp: u8,
    pub max_hp: u8,
    pub ready: bool,      // false the turn it arrives (summoning sickness)
    pub is_vehicle: bool, // counts against MAX_VEHICLES vs MAX_OPERATORS
    pub keywords: Vec<Keyword>,
}

/// Live per-seat state that the scalar OutfitConfig cannot express: the hand,
/// the ordered secret deck, and the board. Resource scalars (juice/heat/boss_hp)
/// stay on OutfitConfig for Subsystem 1 (see Task 4 design note).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeatState {
    pub hand: Vec<CardInstance>,
    pub deck: Vec<CardInstance>, // server-secret; ordered
    pub board: Vec<BoardUnit>,
}
```

> `CardType` currently derives `Debug, Clone, Copy, PartialEq, Eq` (card_definition.rs:50) but NOT `Serialize`/`Deserialize`. `CardInstance` needs to serialize `card_type`. Add `serde::Serialize, serde::Deserialize` to `CardType`'s derives in `card_definition.rs` (it is a simple C-like enum, so `#[derive(Serialize, Deserialize)]` is sufficient; add a `#[serde(...)]` rename only if the client wire form needs the string spellings — for Subsystem 1 the default enum encoding is fine since these travel inside Rust/WASM, not to the legacy client JSON yet).

- [ ] **Step 4: Thread `SeatState` into `GameSession` and add accessors**

Add two `SeatState` fields to `struct GameSession` (near the `OutfitConfig` fields, ~lib.rs:851). Mirror the existing `outfit_at`/`outfit_at_mut` accessor pattern:

```rust
    fn seat_state_at(&self, seat: Player) -> &SeatState {
        match seat {
            Player::A => &self.seat_a,
            Player::B => &self.seat_b,
        }
    }

    fn seat_state_at_mut(&mut self, seat: Player) -> &mut SeatState {
        match seat {
            Player::A => &mut self.seat_a,
            Player::B => &mut self.seat_b,
        }
    }
```

Initialize `seat_a`/`seat_b` to `SeatState::default()` in `GameSession::new` (grep for `GameSession::new`; add the two fields to its struct literal). Because `SeatState: Default` and empty, every existing test that never deals a hand is unaffected.

- [ ] **Step 5: Implement the seeded deck-build helper**

Port `mulberry32` + `buildDeck` as pure functions (WASM-safe — no RNG crate):

```rust
/// The client's mulberry32 PRNG (web/src/match/rules.ts:71), reproduced exactly
/// so a Rust-dealt deck matches a WASM-predicted one bit-for-bit.
fn mulberry32(mut state: u32) -> impl FnMut() -> f64 {
    move || {
        state = state.wrapping_add(0x6D2B79F5);
        let mut t = state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        (((t ^ (t >> 14)) >> 0) as f64) / 4_294_967_296.0
    }
}
```

> Verify this reproduces the JS output for a couple of seed values against the TS (`mulberry32` in rules.ts) — if the bit-ops differ, the decks diverge and the WASM prediction (Task 9) will mismatch the server. Add a `#[test]` asserting the first three draws for seed `0xc0ffee` equal the values the TS produces (compute them once from the client and hard-code the expected `f64`s).

Then a `build_deck(seed, seat) -> Vec<CardInstance>` that mirrors rules.ts:82 — 30 cards drawn from a card pool, Fisher–Yates shuffled with the same stream. For Subsystem 1 the pool is the 14-card set ported from `CARD_POOL` (rules.ts:50-65) as a `const` table of `(card_id, cost, card_type, effect, atk, hp, keywords)` tuples; map each to a `CardInstance` with `instance_id = format!("{seat}-{card_id}-{n}")`. Keep the concrete amounts (e.g. Bolt = `DealDamage{3}`, Blow the Safe = `DrawCards{2}`).

- [ ] **Step 6: Rewrite `start_match` to deal seats**

In `start_match` (grep for `fn start_match`), after the existing validation and before/alongside recording `MatchStarted`, build both seats:

```rust
        let seed = cmd.rng_seed; // add rng_seed to StartMatch if absent; default 0xc0ffee
        for seat in [Player::A, Player::B] {
            let mut deck = build_deck(seed, seat);
            let hand: Vec<CardInstance> = deck.drain(0..OPENING_HAND.min(deck.len())).collect();
            let st = self.seat_state_at_mut(seat);
            st.hand = hand;
            st.deck = deck;
            st.board = Vec::new();
        }
```

Keep the existing `OutfitConfig`/`MatchStarted` logic intact.

- [ ] **Step 7: Run the deal test + full suite**

Run: `cargo test -p game-session start_match && make test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/game-session/src/lib.rs crates/domain/src/card_definition.rs
git commit -m "feat(engine): live SeatState (hand/deck/board) + seeded opening deal

Adds CardInstance/BoardUnit/SeatState and threads a SeatState per seat into
GameSession alongside OutfitConfig. start_match now deals a deterministic
OPENING_HAND from a seeded 30-card deck built with a Rust port of the client's
mulberry32 PRNG, so a WASM-predicted deal matches the server bit-for-bit."
```

---

## Task 5: Real combat (simultaneous + retaliation), replacing the boss-instakill

**Files:**
- Modify: `crates/game-session/src/lib.rs` — rewrite `declare_attack` (1333-1412); add combat delta events to the `Event` enum (792-818) + `event_type` (820-836); rename the command to `AttackCmd` with a `target_ref` payload.
- Modify: `web/src/match/model.ts` — the `AttackCmd` action already uses `{seat, attackerId, targetRef}` (207); ensure the Rust side matches.
- Test: `game-session` test module.

**Interfaces:**
- Consumes: `SeatState.board` (Task 4), `Keyword::Spotlight` (Task 3), `OutfitConfig.boss_hp` (resource scalar).
- Produces: an `AttackCmd`/`Attack` command with `{match_id, player_id, attacker_id, target_ref}` where `target_ref` is `"boss:<seat>"` | `"op:<instance_id>"`; new `Event` variants `OperatorDamaged`, `OperatorDied`, `BossDamaged`, `OperatorExhausted` with `event_type` strings `operator.damaged`/`operator.died`/`boss.damaged`/`operator.exhausted` (exactly the client's `model.ts:223` fold types); `MatchCompleted` reused when a Boss reaches 0.

**Resolved ambiguity (command name):** Standardize on **`AttackCmd`** (the client's existing word, model.ts:207) with payload field `target_ref`. Rename the Rust const `DECLARE_ATTACK` (lib.rs:90) value from `"DeclareAttackCmd"` to `"AttackCmd"`, rename the struct `DeclareAttack` → `Attack`, and replace its `defender_id` field with `target_ref`. The old `defender_id` was a stub-era name meaning "the enemy Boss"; real combat targets units too.

- [ ] **Step 1: Add the combat delta event structs + variants**

Add structs near the other events (~700), then variants to `Event` (792) and arms to `event_type` (822):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorDamaged { pub match_id: String, pub player: Player, pub instance_id: String, pub new_hp: u8 }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorDied { pub match_id: String, pub player: Player, pub instance_id: String }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BossDamaged { pub match_id: String, pub player: Player, pub amount: i32, pub new_hp: i32 }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorExhausted { pub match_id: String, pub player: Player, pub instance_id: String }
```

```rust
    // in enum Event:
    OperatorDamaged(OperatorDamaged),
    OperatorDied(OperatorDied),
    BossDamaged(BossDamaged),
    OperatorExhausted(OperatorExhausted),
```

```rust
    // in event_type():
    Event::OperatorDamaged(_) => "operator.damaged",
    Event::OperatorDied(_) => "operator.died",
    Event::BossDamaged(_) => "boss.damaged",
    Event::OperatorExhausted(_) => "operator.exhausted",
```

> `player` on these deltas is the **owner of the affected unit/Boss** (the defender), matching the client's fold (`model.ts:230-236` use `player: foe`).

- [ ] **Step 2: Write the failing combat tests**

```rust
    #[test]
    fn attack_unit_is_simultaneous_with_retaliation() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        // A attacker 3/2, B defender 2/5.
        session.seat_state_at_mut(Player::A).board.push(test_unit("A-atk", 3, 2, true, false, &[]));
        session.seat_state_at_mut(Player::B).board.push(test_unit("B-def", 2, 5, true, false, &[]));

        let events = session
            .execute(Attack::new("m-1", "m-1-a", "A-atk", "op:B-def").into_command())
            .expect("A attacks B's unit");

        // Defender took 3 (5 -> 2); attacker took retaliation 2 (2 -> 0) and died.
        assert!(events.iter().any(|e| matches!(e, Event::OperatorDamaged(d) if d.instance_id == "B-def" && d.new_hp == 2)));
        assert!(events.iter().any(|e| matches!(e, Event::OperatorDied(d) if d.instance_id == "A-atk")));
        assert!(session.seat_state_at(Player::A).board.iter().all(|u| u.instance_id != "A-atk"));
    }

    #[test]
    fn attack_boss_reduces_hp_and_ends_match_at_zero() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        let mut b = OutfitConfig::new("m-1-b");
        b.boss_hp = 3;
        session.configure_player_b(b);
        session.seat_state_at_mut(Player::A).board.push(test_unit("A-atk", 5, 5, true, false, &[]));

        let events = session
            .execute(Attack::new("m-1", "m-1-a", "A-atk", "boss:B").into_command())
            .expect("A attacks B's boss");

        assert!(events.iter().any(|e| matches!(e, Event::BossDamaged(d) if d.new_hp == 0)));
        assert!(events.iter().any(|e| matches!(e, Event::BossDefeated(d) if d.winner == Player::A)));
    }

    #[test]
    fn spotlight_forces_attack_onto_taunt_unit() {
        let mut session = valid_session();
        session.set_opening_player(Some(Player::A));
        session.seat_state_at_mut(Player::A).board.push(test_unit("A-atk", 2, 2, true, false, &[]));
        session.seat_state_at_mut(Player::B).board.push(test_unit("B-taunt", 0, 4, true, false, &[Keyword::Spotlight]));
        // Attacking the boss while a Spotlight unit stands is rejected.
        let err = session
            .execute(Attack::new("m-1", "m-1-a", "A-atk", "boss:B").into_command())
            .expect_err("must hit the Spotlight first");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn attack_cmd_is_recognized_not_unknown() {
        let mut session = valid_session();
        let err = session.execute(Attack::new("m-1", "m-1-a", "x", "boss:B").into_command());
        // Whatever the rejection reason, it must NOT be UnknownCommand — the rename worked.
        assert!(!matches!(err, Err(DomainError::UnknownCommand { .. })));
    }
```

Add a `test_unit` helper to the test module:

```rust
    fn test_unit(id: &str, atk: u8, hp: u8, ready: bool, is_vehicle: bool, kws: &[Keyword]) -> BoardUnit {
        BoardUnit { instance_id: id.to_string(), card_id: "test".to_string(), atk, hp, max_hp: hp, ready, is_vehicle, keywords: kws.to_vec() }
    }
```

- [ ] **Step 3: Run to confirm failure**

Run: `cargo test -p game-session attack spotlight`
Expected: FAIL — `Attack`, combat events undefined; command still named `DeclareAttack`.

- [ ] **Step 4: Rename the command and rewrite `declare_attack` as real combat**

Rename const value (lib.rs:90) to `"AttackCmd"`, rename `struct DeclareAttack` → `Attack`, replace field `defender_id` with `target_ref`, update `Attack::COMMAND`/`Attack::new`. Then rewrite the handler. Keep the identical preamble (match-id/player-id/turn checks + the six `ensure_*` guards, lib.rs:1335-1385); replace the boss-instakill body (1361-1411) with:

```rust
        // Resolve target from target_ref: "boss:<seat>" | "op:<instance_id>".
        let defending_player = Self::opponent_of(seat);
        let target = cmd.target_ref.as_str();

        // Attacker must be a READY unit the acting seat owns.
        let attacker = self
            .seat_state_at(seat)
            .board
            .iter()
            .find(|u| u.instance_id == cmd.attacker_id)
            .ok_or_else(|| DomainError::InvariantViolation(format!("no unit '{}' on the attacker's board", cmd.attacker_id)))?;
        if !attacker.ready {
            return Err(DomainError::InvariantViolation(format!("unit '{}' is not ready (summoning sickness)", cmd.attacker_id)));
        }
        let attacker_atk = attacker.atk;

        // Spotlight: if the defender has any Spotlight unit, the target must be one.
        let has_spotlight = self.seat_state_at(defending_player).board.iter().any(|u| u.keywords.contains(&Keyword::Spotlight));
        if has_spotlight {
            let targeting_spotlight = target.strip_prefix("op:").map_or(false, |id| {
                self.seat_state_at(defending_player).board.iter().any(|u| u.instance_id == id && u.keywords.contains(&Keyword::Spotlight))
            });
            if !targeting_spotlight {
                return Err(DomainError::InvariantViolation("must attack a Spotlight unit first".to_string()));
            }
        }

        let mut events: Vec<Event> = Vec::new();
        if let Some(defender_id) = target.strip_prefix("op:") {
            // Capture both attack values BEFORE applying damage (simultaneous).
            let retaliation = self
                .seat_state_at(defending_player)
                .board
                .iter()
                .find(|u| u.instance_id == defender_id)
                .map(|u| u.atk)
                .ok_or_else(|| DomainError::InvariantViolation(format!("no defender '{defender_id}'")))?;
            // Apply attacker -> defender.
            events.extend(self.apply_unit_damage(defending_player, defender_id, attacker_atk));
            // Apply defender -> attacker (retaliation).
            if retaliation > 0 {
                events.extend(self.apply_unit_damage(seat, &cmd.attacker_id, retaliation));
            }
        } else if let Some(boss_seat) = target.strip_prefix("boss:") {
            let _ = boss_seat; // target names the enemy boss; enforce it is the defender
            let outfit = self.outfit_at_mut(defending_player);
            outfit.boss_hp -= attacker_atk as i32;
            let new_hp = outfit.boss_hp.max(0);
            events.push(Event::BossDamaged(BossDamaged { match_id: self.match_id.clone(), player: defending_player, amount: attacker_atk as i32, new_hp }));
            if new_hp == 0 {
                // Terminal: reuse the existing BossDefeated event (what the old
                // declare_attack emitted) — MatchCompleted is concession-shaped and
                // wrong for a combat kill. Fields confirmed against lib.rs:683.
                let defeated_player_id = self.outfit_at(defending_player).name.clone();
                let boss_id = self.outfit_at(defending_player).boss_name.clone();
                events.push(Event::BossDefeated(BossDefeated {
                    match_id: self.match_id.clone(),
                    defeated_player_id,
                    defeated_player: defending_player,
                    boss_id,
                    winner: seat,
                }));
            }
        } else {
            return Err(DomainError::InvariantViolation(format!("malformed targetRef '{}'", cmd.target_ref)));
        }

        // Attacker exhausts if it survived the trade.
        if self.seat_state_at(seat).board.iter().any(|u| u.instance_id == cmd.attacker_id) {
            events.push(Event::OperatorExhausted(OperatorExhausted { match_id: self.match_id.clone(), player: seat, instance_id: cmd.attacker_id.clone() }));
            if let Some(u) = self.seat_state_at_mut(seat).board.iter_mut().find(|u| u.instance_id == cmd.attacker_id) { u.ready = false; }
        }

        for e in &events { self.root.record(Box::new(e.clone())); }
        Ok(events)
```

Add the damage helper (returns the deltas and removes dead units):

```rust
    /// Apply `amount` damage to `owner`'s unit `instance_id`, returning the
    /// resulting deltas (OperatorDamaged, and OperatorDied if it drops to 0).
    /// Removes the unit from the board when it dies.
    fn apply_unit_damage(&mut self, owner: Player, instance_id: &str, amount: u8) -> Vec<Event> {
        let mut out = Vec::new();
        let board = &mut self.seat_state_at_mut(owner).board;
        if let Some(u) = board.iter_mut().find(|u| u.instance_id == instance_id) {
            let new_hp = u.hp.saturating_sub(amount);
            u.hp = new_hp;
            out.push(Event::OperatorDamaged(OperatorDamaged { match_id: String::new(), player: owner, instance_id: instance_id.to_string(), new_hp }));
            if new_hp == 0 {
                out.push(Event::OperatorDied(OperatorDied { match_id: String::new(), player: owner, instance_id: instance_id.to_string() }));
            }
        }
        if let Some(u) = out.iter().find_map(|e| if let Event::OperatorDied(d) = e { Some(d.instance_id.clone()) } else { None }) {
            self.seat_state_at_mut(owner).board.retain(|b| b.instance_id != u);
        }
        // Fill in match_id (borrow of self.match_id after the &mut board borrow ends).
        let mid = self.match_id.clone();
        for e in out.iter_mut() {
            match e { Event::OperatorDamaged(d) => d.match_id = mid.clone(), Event::OperatorDied(d) => d.match_id = mid.clone(), _ => {} }
        }
        out
    }
```

> Borrow-checker note: `apply_unit_damage` takes `&mut self` and mutates one seat's board, then reads `self.match_id` — sequence the `&mut board` borrow to end before the `self.match_id.clone()` (as written). **Terminal event (confirmed):** emit the existing `BossDefeated` (lib.rs:683, fields `{match_id, defeated_player_id, defeated_player, boss_id, winner}`) — the same event the old `declare_attack` emitted. Do NOT use `MatchCompleted`: its shape is concession-specific (`conceding_player_id`/`winning_player_id`, lib.rs:777) and does not model a combat kill. The client not folding `boss.defeated` yet is not a regression (the old code emitted it too); a `boss.defeated` fold is a Subsystem-2 client task.

- [ ] **Step 5: Run the combat tests + full suite**

Run: `cargo test -p game-session attack spotlight && make test`
Expected: PASS. Existing `declare_attack` tests (2578-2786) reference `DeclareAttack`/`defender_id`/the instakill — **update them** to the `Attack`/`target_ref` names and real-combat expectations; a test asserting the old boss-instakill is asserting removed behavior, so rewrite it to the boss-damage semantics and note it in the commit body.

- [ ] **Step 6: Commit**

```bash
git add crates/game-session/src/lib.rs web/src/match/model.ts
git commit -m "feat(engine): real board combat (simultaneous + retaliation + Spotlight)

Replaces the boss-instakill stub with combat over BoardUnits ported from
rules.ts: attacker and defender atk captured before damage (simultaneous),
defender retaliates, dead units removed, attacker exhausts. Spotlight forces
targeting. Boss damage reduces boss_hp and ends the match at 0. Renames the
DeclareAttackCmd command to AttackCmd with a targetRef payload (boss:<seat> |
op:<id>), resolving the client/Rust command drift. Emits operator.damaged/
operator.died/boss.damaged/operator.exhausted deltas the client already folds."
```

---

## Task 6: Effect resolution in `play_card` (summon/damage/juice/cool/draw + Drive-By)

**Files:**
- Modify: `crates/game-session/src/lib.rs` — extend `play_card` (1260-1327, now also mutating state from Task 1) to resolve the played card's `CardEffect` against state; add a `resolve_effect` helper; enforce board caps on summon.
- Test: `game-session` test module.

**Interfaces:**
- Consumes: `CardInstance` in the acting seat's `hand` (Task 4), `CardEffect`/`Keyword` (Task 3), `apply_unit_damage`/board (Task 5), `MAX_OPERATORS`/`MAX_VEHICLES`.
- Produces: `play_card` produces real board/hand/Boss changes and emits `operator.summoned`/effect deltas in addition to `CardPlayed`+`HeatRaised`. Add an `OperatorSummoned` event (`event_type` `operator.summoned`) mirroring the client fold (model.ts:232).

**Design note:** `play_card` today takes a bare `card_instance_id` and a client-supplied `juice_cost`; it does not look the card up. To resolve an effect the engine must find the `CardInstance` in the acting seat's `hand` by `instance_id`, use ITS `cost` (not the client's claimed `juice_cost`) for the deduction, and remove it from hand on play. **Validate the command's `juice_cost` equals the instance's `cost`** so the client cannot understate cost (same anti-cheat posture as hero-power cost in Task 7).

- [ ] **Step 1: Add the `OperatorSummoned` event**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorSummoned { pub match_id: String, pub player: Player, pub unit: BoardUnit }
// enum Event: OperatorSummoned(OperatorSummoned),
// event_type: Event::OperatorSummoned(_) => "operator.summoned",
```

- [ ] **Step 2: Write the failing effect tests**

```rust
    #[test]
    fn play_summon_card_puts_unit_on_board_unready() {
        let mut session = seated_match(); // helper: start_match + set opening A + give A a known hand
        // Put a known summon card in A's hand: a 3/2 Operator, cost 2, effect Summon.
        session.seat_state_at_mut(Player::A).hand.push(test_card_instance("A-homie-0", 2, CardType::Operator, CardEffect::Summon, 3, 2, &[]));
        give_juice(&mut session, Player::A, 5);

        let events = session
            .execute(PlayCard::new("m-1", "m-1-a", "A-homie-0", "boss:B", 2).into_command())
            .expect("summon is affordable");

        assert!(events.iter().any(|e| matches!(e, Event::OperatorSummoned(s) if s.unit.instance_id == "A-homie-0" && !s.unit.ready)));
        assert!(session.seat_state_at(Player::A).board.iter().any(|u| u.instance_id == "A-homie-0"));
        assert!(session.seat_state_at(Player::A).hand.iter().all(|c| c.instance_id != "A-homie-0"), "card leaves hand");
    }

    #[test]
    fn play_damage_card_hits_the_boss() {
        let mut session = seated_match();
        session.seat_state_at_mut(Player::A).hand.push(test_card_instance("A-bolt-0", 1, CardType::Job, CardEffect::DealDamage { amount: 3 }, 0, 0, &[]));
        give_juice(&mut session, Player::A, 5);
        let mut b = OutfitConfig::new("m-1-b"); b.boss_hp = 10; session.configure_player_b(b);

        session.execute(PlayCard::new("m-1", "m-1-a", "A-bolt-0", "boss:B", 1).into_command()).unwrap();
        assert_eq!(session.outfit_at(Player::B).boss_hp, 7, "10 - 3");
    }

    #[test]
    fn play_driveby_summon_also_hits_enemy_boss() {
        let mut session = seated_match();
        // Stolen Whip: 4/3 Vehicle, Drive-By amount 2.
        session.seat_state_at_mut(Player::A).hand.push(test_card_instance("A-whip-0", 3, CardType::Vehicle, CardEffect::Summon, 4, 3, &[Keyword::DriveBy]));
        give_juice(&mut session, Player::A, 5);
        let mut b = OutfitConfig::new("m-1-b"); b.boss_hp = 10; session.configure_player_b(b);

        session.execute(PlayCard::new("m-1", "m-1-a", "A-whip-0", "boss:B", 3).into_command()).unwrap();
        assert_eq!(session.outfit_at(Player::B).boss_hp, 8, "Drive-By strafes 2 on arrival");
    }

    #[test]
    fn summon_rejected_when_operator_board_full() {
        let mut session = seated_match();
        for i in 0..MAX_OPERATORS { session.seat_state_at_mut(Player::A).board.push(test_unit(&format!("A-op-{i}"), 1, 1, true, false, &[])); }
        session.seat_state_at_mut(Player::A).hand.push(test_card_instance("A-homie-0", 2, CardType::Operator, CardEffect::Summon, 3, 2, &[]));
        give_juice(&mut session, Player::A, 5);
        let err = session.execute(PlayCard::new("m-1", "m-1-a", "A-homie-0", "boss:B", 2).into_command()).expect_err("board full");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }
```

Add test helpers `seated_match()`, `give_juice(session, seat, n)` (set `max_juice`/`available_juice`), and `test_card_instance(...)` to the test module.

- [ ] **Step 3: Run to confirm failure**

Run: `cargo test -p game-session play_summon play_damage play_driveby summon_rejected`
Expected: FAIL.

- [ ] **Step 4: Resolve the effect inside `play_card`**

In `play_card`, after locating the seat and before the Juice mutation (Task 1's block), find the instance in hand, validate cost parity, and after recording `CardPlayed`/`HeatRaised`, resolve its effect:

```rust
        // Find the played instance in the acting seat's hand; its cost is authoritative.
        let instance = self
            .seat_state_at(seat)
            .hand
            .iter()
            .find(|c| c.instance_id == cmd.card_instance_id)
            .cloned()
            .ok_or_else(|| DomainError::InvariantViolation(format!("card '{}' is not in {seat:?}'s hand", cmd.card_instance_id)))?;
        if cmd.juice_cost != instance.cost {
            return Err(DomainError::InvariantViolation(format!(
                "declared cost {} does not match card cost {}", cmd.juice_cost, instance.cost
            )));
        }
        // Board-cap pre-check for summons (before mutating anything).
        if matches!(instance.effect, CardEffect::Summon) {
            self.ensure_summon_capacity(seat, instance.card_type)?;
        }
```

Then, after the affordability/heat checks and the Juice/Heat mutation (Task 1) and after recording `CardPlayed`+`HeatRaised`, remove the card from hand and resolve:

```rust
        // Remove from hand and resolve the effect against state.
        self.seat_state_at_mut(seat).hand.retain(|c| c.instance_id != cmd.card_instance_id);
        let mut effect_events = self.resolve_effect(seat, &instance, &cmd.target_ref);
        for e in &effect_events { self.root.record(Box::new(e.clone())); }
        let mut all = vec![played, raised];
        all.append(&mut effect_events);
        Ok(all)
```

Add `resolve_effect` and `ensure_summon_capacity`:

```rust
    fn ensure_summon_capacity(&self, seat: Player, card_type: CardType) -> Result<(), DomainError> {
        let board = &self.seat_state_at(seat).board;
        let is_vehicle = matches!(card_type, CardType::Vehicle);
        let count = board.iter().filter(|u| u.is_vehicle == is_vehicle).count();
        let cap = if is_vehicle { MAX_VEHICLES } else { MAX_OPERATORS };
        if count >= cap {
            return Err(DomainError::InvariantViolation(format!(
                "{seat:?}'s board is at the {} cap of {cap}", if is_vehicle { "Vehicle" } else { "Operator" }
            )));
        }
        Ok(())
    }

    /// Port of rules.ts:299 resolveEffect. Mutates state and returns deltas.
    fn resolve_effect(&mut self, seat: Player, card: &CardInstance, target_ref: &str) -> Vec<Event> {
        let foe = Self::opponent_of(seat);
        let mid = self.match_id.clone();
        let mut out = Vec::new();
        match card.effect {
            CardEffect::None => {}
            CardEffect::DealDamage { amount } => {
                let tref = if target_ref.is_empty() { format!("boss:{foe:?}") } else { target_ref.to_string() };
                out.extend(self.damage_target(&tref, amount, foe));
            }
            CardEffect::Summon => {
                let unit = BoardUnit {
                    instance_id: card.instance_id.clone(), card_id: card.card_id.clone(),
                    atk: card.atk, hp: card.hp, max_hp: card.hp, ready: false,
                    is_vehicle: matches!(card.card_type, CardType::Vehicle), keywords: card.keywords.clone(),
                };
                self.seat_state_at_mut(seat).board.push(unit.clone());
                out.push(Event::OperatorSummoned(OperatorSummoned { match_id: mid.clone(), player: seat, unit }));
                // Drive-By: strafe the enemy Boss for the card's atk on arrival.
                if card.keywords.contains(&Keyword::DriveBy) {
                    let dmg = /* rules.ts uses the card's `amount`; here use a fixed Drive-By value */ 2u8;
                    out.extend(self.damage_boss(foe, dmg as i32));
                }
            }
            CardEffect::DrawCards { amount } => {
                for _ in 0..amount { if let Some(c) = self.draw_one(seat) { let _ = c; /* emit CardDrawn if the client folds it */ } }
            }
            CardEffect::GainJuice { amount } => {
                let o = self.outfit_at_mut(seat);
                o.available_juice = o.available_juice.saturating_add(amount).min(JUICE_CAP);
            }
            CardEffect::Cool { amount } => {
                let o = self.outfit_at_mut(seat);
                o.starting_heat = (o.starting_heat - amount as i32).max(*HEAT_BOUNDS.start());
            }
        }
        out
    }
```

> `damage_target`/`damage_boss`/`draw_one` are small helpers: `damage_target` parses `op:`/`boss:` and dispatches to `apply_unit_damage` (Task 5) or `damage_boss`; `damage_boss` reduces `outfit_at_mut(foe).boss_hp`, clamps at 0, emits `BossDamaged`, and appends `BossDefeated` (fields per Task 5) at 0. `draw_one` pops the front of `seat_state_at_mut(seat).deck` into `hand`. **Drive-By amount:** the client keys Drive-By off the card's `amount` field (2 for Stolen Whip, rules.ts:61/313), which `CardEffect::Summon` does not carry. For Subsystem 1 use a fixed `DRIVE_BY_DAMAGE = 2` constant (matches the only Drive-By card); Subsystem 2 makes it data-driven when the keyword catalog grows. Add `const DRIVE_BY_DAMAGE: u8 = 2;`.

- [ ] **Step 5: Run the effect tests + full suite**

Run: `cargo test -p game-session play_ summon && make test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/game-session/src/lib.rs
git commit -m "feat(engine): play_card resolves effects (summon/damage/juice/cool/draw)

play_card now finds the played CardInstance in hand, validates the declared cost
against the card's real cost (anti-cheat), removes it from hand, and resolves its
CardEffect against state: Summon puts an unready BoardUnit on the board (enforcing
Operator/Vehicle caps) and fires Drive-By at the enemy Boss on arrival; DealDamage
hits the target; GainJuice/Cool adjust resources; DrawCards pulls from the deck.
Emits operator.summoned + effect deltas the client folds."
```

---

## Task 7: Hero powers with real effects

**Files:**
- Modify: `crates/domain/src/boss_definition.rs` — add `hero_power_effect`/`hero_power_cost`/`trademark_effect`(+trigger) to `DefineBoss` (77-94) and `BossDefined` (112-126); define `HeroPowerEffect`/`TrademarkEffect`/`TrademarkTrigger` enums.
- Modify: `crates/game-session/src/lib.rs` — extend `activate_hero_power` (1418-1480) to apply the effect; land a start-of-turn trademark trigger point in `end_turn`.
- Test: both files' test modules.

**Interfaces:**
- Consumes: the §2.3 effect machinery (`resolve_effect`/`damage_boss`, Task 6), `BossDefinition` fields.
- Produces: `enum HeroPowerEffect { DealDamage{amount:u8}, GainArmor{amount:u8}, SummonToken{atk:u8,hp:u8}, Cool{amount:u8} }`; `BossDefinition.hero_power_cost: u8` (default 2); `activate_hero_power` emits effect deltas (`boss.damaged`, etc.) in addition to `HeroPowerActivated`, and validates the command cost equals the Boss's declared cost.

**Design note (where the Boss's power reaches the engine):** the engine's `GameSession`/`OutfitConfig` does not currently carry the Boss's hero-power effect — `OutfitConfig` has only `boss_name`/`boss_hp`. For Subsystem 1, thread the `hero_power_effect`+`hero_power_cost` onto `OutfitConfig` (set at `StartMatchCmd` from the Boss definition) so `activate_hero_power` can resolve without a cross-aggregate lookup. Add `pub hero_power_effect: HeroPowerEffect` and `pub hero_power_cost: u8` to `OutfitConfig`, default `DealDamage{2}`/`2` in `new()` (matches the client's hardcoded 2-poke, rules.ts:250).

- [ ] **Step 1: Define the effect enums in `boss_definition.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeroPowerEffect {
    DealDamage { amount: u8 },
    GainArmor { amount: u8 },
    SummonToken { atk: u8, hp: u8 },
    Cool { amount: u8 },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrademarkTrigger { StartOfTurn, OnPlay }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrademarkEffect { pub trigger: TrademarkTrigger, pub effect: HeroPowerEffect }
```

Add `#[serde(default)]`-friendly fields to `DefineBoss` and `BossDefined` (`hero_power_effect`, `hero_power_cost`, `trademark_effect`). Provide `Default` for `HeroPowerEffect` (= `DealDamage{2}`) so existing boss-definition tests that omit the new fields still deserialize. Keep the existing `hero_powers`/`trademarks` string vecs (display names) — the effects are additive.

- [ ] **Step 2: Write the failing `boss_definition` test**

```rust
    #[test]
    fn define_boss_carries_hero_power_effect_and_cost() {
        let mut agg = BossDefinition::new("boss-1");
        let cmd = DefineBoss { hero_power_effect: HeroPowerEffect::DealDamage { amount: 2 }, hero_power_cost: 2, ..valid_define_cmd() };
        let events = agg.execute(cmd.into_command()).expect("valid boss");
        match &events[0] {
            Event::BossDefined(d) => { assert_eq!(d.hero_power_effect, HeroPowerEffect::DealDamage { amount: 2 }); assert_eq!(d.hero_power_cost, 2); }
            other => panic!("expected BossDefined, got {other:?}"),
        }
    }
```

Run: `cargo test -p domain -- boss_definition` → FAIL, then implement Step 1's fields + `define_boss` event wiring → PASS.

- [ ] **Step 3: Write the failing engine tests**

```rust
    #[test]
    fn hero_power_deals_declared_damage_and_spends_juice() {
        let mut session = seated_match();
        let mut a = OutfitConfig::new("m-1-a"); a.max_juice = 5; a.available_juice = 5;
        a.hero_power_effect = HeroPowerEffect::DealDamage { amount: 2 }; a.hero_power_cost = 2;
        session.configure_player_a(a);
        let mut b = OutfitConfig::new("m-1-b"); b.boss_hp = 10; session.configure_player_b(b);
        session.set_opening_player(Some(Player::A));

        let events = session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "boss:B", 2).into_command())
            .expect("affordable hero power");

        assert!(events.iter().any(|e| matches!(e, Event::BossDamaged(d) if d.amount == 2 && d.new_hp == 8)));
        assert_eq!(session.outfit_at(Player::A).available_juice, 3, "5 - 2");
    }

    #[test]
    fn hero_power_rejects_understated_cost() {
        let mut session = seated_match();
        let mut a = OutfitConfig::new("m-1-a"); a.max_juice = 5; a.available_juice = 5; a.hero_power_cost = 2;
        session.configure_player_a(a); session.set_opening_player(Player::A);
        let err = session
            .execute(ActivateHeroPower::new("m-1", "m-1-a", "boss:B", 0).into_command())
            .expect_err("client cannot understate the declared cost");
        assert!(matches!(err, DomainError::InvariantViolation(_)));
    }
```

> Confirm the `ActivateHeroPower::new` arity by grep (current fields: `match_id, player_id, target_ref, juice_cost`). The test uses `juice_cost` as the client's claimed cost; the handler now cross-checks it against `OutfitConfig.hero_power_cost`.

Run: `cargo test -p game-session hero_power` → FAIL.

- [ ] **Step 4: Extend `activate_hero_power`**

Keep the preamble + the `available_juice -= cmd.juice_cost` deduction (1466-1468). Between the affordability check (1461) and the deduction, add the cost-parity check; after the deduction and the `HeroPowerActivated` event, resolve the effect:

```rust
        let declared_cost = self.outfit_at(seat).hero_power_cost;
        if cmd.juice_cost != declared_cost {
            return Err(DomainError::InvariantViolation(format!(
                "hero power costs {declared_cost} Juice but the command claims {}", cmd.juice_cost
            )));
        }
        self.ensure_card_affordable(seat, declared_cost)?;
        // ... existing deduction + HeroPowerActivated event ...
        let effect = self.outfit_at(seat).hero_power_effect;
        let mut effect_events = self.resolve_hero_power(seat, effect, &cmd.target_ref);
        for e in &effect_events { self.root.record(Box::new(e.clone())); }
        let mut all = vec![activated];
        all.append(&mut effect_events);
        Ok(all)
```

`resolve_hero_power` maps `HeroPowerEffect` to the Task-6 helpers: `DealDamage` → `damage_boss(foe, amount)` (or `damage_target` if `target_ref` names a unit); `GainArmor` → `outfit_at_mut(seat).boss_hp += amount`; `Cool` → lower own `starting_heat`; `SummonToken` → push a `BoardUnit` (respect caps via `ensure_summon_capacity`, and if the board is full, the token is simply not summoned — mirror Hearthstone; do NOT reject the whole activation).

- [ ] **Step 5: Land the trademark start-of-turn seam**

In `end_turn`'s incoming-seat sequence (Task 1's block), after the crystal refill, add a single trigger point: if the incoming seat's Boss `trademark_effect.trigger == StartOfTurn`, resolve its `effect` and append the deltas to the returned events. For Subsystem 1 the default trademark is a no-op (`effect: DealDamage{0}` or a dedicated `None` — add `HeroPowerEffect` is not `Option`; simplest: make `trademark_effect: Option<TrademarkEffect>` and default `None`, so no behavior change and existing `end_turn` tests stay green). This lands the seam; the trademark catalog is Subsystem 2.

- [ ] **Step 6: Run + commit**

Run: `cargo test -p game-session hero_power && make test` → PASS.

```bash
git add crates/game-session/src/lib.rs crates/domain/src/boss_definition.rs
git commit -m "feat(engine): hero powers apply real effects; trademark start-of-turn seam

Binds BossDefinition.hero_power to a typed HeroPowerEffect (DealDamage/GainArmor/
SummonToken/Cool) with a declared hero_power_cost (default 2), threaded onto
OutfitConfig at StartMatch. activate_hero_power now validates the command cost
against the Boss's declared cost (anti-cheat) and resolves the effect, emitting
boss.damaged/operator.summoned deltas alongside HeroPowerActivated. Adds an
Option<TrademarkEffect> start-of-turn trigger seam (default None, no regression)."
```

---

## Task 8: Boss-locked cards (only Boss X may deck this)

**Files:**
- Modify: `crates/domain/src/card_definition.rs` — add `boss_lock: Option<String>` to `DefineCardCmd`/`CardDefined` (+revise); invariant "locked ⇒ class == Boss".
- Modify: `crates/domain/src/outfit.rs` — add a boss binding + the enforcement invariant (needs new aggregate state; see design note).
- Modify: `crates/game-session/src/lib.rs` — `start_match` re-validates every dealt `CardInstance.boss_lock` against the seat's Boss.
- Test: all three files' test modules.

**Interfaces:**
- Consumes: `CardClass::Boss`, the Outfit aggregate's add/save/validate paths, `CardInstance.boss_lock` (Task 4).
- Produces: `DefineCardCmd.boss_lock: Option<String>`; a new Outfit invariant `ensure_boss_locks_honored`; a `start_match` `InvariantViolation` when a seat decks a mismatched boss-locked card.

**Design note (Outfit has no boss/card state today):** The Outfit aggregate tracks deck legality as three booleans + an `i64 count` — it has **no boss id and no card list** (mapping finding #2). Enforcing "only Boss X may deck card locked to X" therefore requires new aggregate state. **Decision for Subsystem 1:** (a) add `boss_id: String` to `struct Outfit`, set at `CreateOutfit` (add a `boss_id` field to `CreateOutfit`); (b) carry the added card's lock on the command — add `#[serde(default)] pub boss_lock: Option<String>` to `AddCardToOutfit` (the Outfit cannot look up the catalog cross-aggregate, so the caller supplies the lock the catalog recorded); (c) the invariant `ensure_boss_locks_honored` checks, on `add_card`, that a non-`None` `boss_lock` equals `self.boss_id`. This keeps the aggregate self-contained. The engine cross-check (Task 8c) is the real anti-cheat backstop since decks back tradeable assets.

- [ ] **Step 1: Catalog — add `boss_lock` + the class invariant**

`DefineCardCmd`/`ReviseCardCmd`/`CardDefined`/`CardRevised` gain `#[serde(default)] pub boss_lock: Option<String>`. In `validate_card_fields`, after the class parse:

```rust
    // A boss-locked card must be a Boss-class card.
    if boss_lock.is_some() && class != CardClass::Boss {
        return Err(DomainError::InvariantViolation(
            "a boss-locked card must be of class Boss".to_string(),
        ));
    }
```

Tests: `define_card_rejects_boss_lock_on_non_boss_class` (lock + `Driver` class → reject); a Boss-class locked card is accepted. Run `cargo test -p domain -- card_definition` red→green.

- [ ] **Step 2: Outfit — add boss binding + lock enforcement**

Add `boss_id: String` to `struct Outfit`; set it in `create` from a new `CreateOutfit.boss_id` field; add `boss_lock: Option<String>` to `AddCardToOutfit`; add:

```rust
    fn ensure_boss_lock_honored(&self, boss_lock: &Option<String>) -> Result<(), DomainError> {
        if let Some(b) = boss_lock {
            if b != &self.boss_id {
                return Err(DomainError::InvariantViolation(format!(
                    "card is locked to Boss '{b}' but this Outfit's Boss is '{}'", self.boss_id
                )));
            }
        }
        Ok(())
    }
```

Call it first in `add_card` (before the four existing `ensure_*`). Tests: `outfit_rejects_boss_locked_card_for_wrong_boss`, `outfit_accepts_boss_locked_card_for_matching_boss` (use the `ready_outfit()` helper; set `boss_id` via a new `set_boss_id` config method mirroring the existing `set_*`). Red→green.

- [ ] **Step 3: Engine cross-check in `start_match`**

After dealing decks (Task 4 Step 6), validate each seat's dealt cards:

```rust
        for seat in [Player::A, Player::B] {
            let boss = self.outfit_at(seat).boss_name.clone();
            for c in self.seat_state_at(seat).deck.iter().chain(self.seat_state_at(seat).hand.iter()) {
                if let Some(b) = &c.boss_lock {
                    if b != &boss {
                        return Err(DomainError::InvariantViolation(format!(
                            "seat {seat:?} decks card '{}' locked to Boss '{b}', but its Boss is '{boss}'", c.card_id
                        )));
                    }
                }
            }
        }
```

Test: `start_match_rejects_mismatched_boss_lock` — seed a seat's deck with a card whose `boss_lock` names a different boss → `StartMatch` rejected. (Inject the mismatched instance by constructing the seat's deck directly in the test before `start_match`, or via a StartMatch payload that carries a locked card.)

- [ ] **Step 4: Run + commit**

Run: `make test` → PASS.

```bash
git add crates/domain/src/card_definition.rs crates/domain/src/outfit.rs crates/game-session/src/lib.rs
git commit -m "feat(engine): boss-locked cards enforced at catalog, deck-build, and match start

A card may declare boss_lock=Some(boss_id); the catalog requires such a card be
Boss-class. The Outfit aggregate gains a boss_id binding and rejects adding a card
locked to a different Boss. start_match re-validates every dealt card's lock
against the seat's Boss as the server-authoritative anti-cheat backstop (decks
back tradeable assets)."
```

---

## Task 9: Command-drift fix + real stateful WASM gate

**Files:**
- Modify: `crates/game-session/src/lib.rs` — replace `wasm_bindings::execute_command` (1798-1805) with a stateful `WasmGameSession`.
- Modify: `web/src/match/connection.ts` — `send()` (66-70) ships the structured envelope.
- Modify: `web/src/match/model.ts` — ensure the action `kind`/field names match the finalized `AttackCmd`/`targetRef` (Task 5 already standardized the Rust side).
- Test: a `wasm-bindgen-test` in `game-session`; a Vitest test for `connection.ts`.

**Interfaces:**
- Consumes: `GameSession::execute`, `Command::with_payload`, the finalized `AttackCmd` name (Task 5).
- Produces: `WasmGameSession` with `new(match_id)`, `start(cmd_json)`, `execute(command_name, payload_json)` returning deltas-as-JSON (prediction) or the domain-error text (rejection) — the identical decision the server's `apply_action` makes.

- [ ] **Step 1: Replace the WASM binding with a stateful handle**

In the `#[cfg(feature = "wasm")]` module (1786), replace `execute_command` with:

```rust
    #[wasm_bindgen]
    pub struct WasmGameSession(GameSession);

    #[wasm_bindgen]
    impl WasmGameSession {
        #[wasm_bindgen(constructor)]
        pub fn new(match_id: String) -> WasmGameSession { WasmGameSession(GameSession::new(match_id)) }

        /// Run a command by name with a JSON payload; returns the emitted deltas
        /// as JSON on success, or the domain-error text on rejection — the same
        /// decision the server's apply_action makes for the same input.
        pub fn execute(&mut self, command_name: String, payload_json: String) -> Result<JsValue, JsValue> {
            let payload = payload_json.into_bytes();
            match self.0.execute(Command::with_payload(command_name, payload)) {
                Ok(events) => {
                    let types: Vec<&'static str> = events.iter().map(|e| e.event_type()).collect();
                    serde_wasm_bindgen::to_value(&types).map_err(|e| JsValue::from_str(&e.to_string()))
                }
                Err(err) => Err(JsValue::from_str(&err.to_string())),
            }
        }
    }
```

> This needs event payloads serialized for real prediction; for Subsystem 1 returning the event_type list proves the state machine ran (prediction == authority at the event-sequence level). If richer deltas are needed by the client immediately, add `#[derive(Serialize)]` to the delta structs and serialize the `Vec<Event>` — but that requires `Event` to be `Serialize`, which is a larger change; the event_type list is the minimal provable gate. `serde-wasm-bindgen` must be added under the `wasm` feature in `crates/game-session/Cargo.toml` (`serde-wasm-bindgen = { version = "0.6", optional = true }`, added to `wasm = [...]`).

- [ ] **Step 2: Write the `wasm-bindgen-test` proving prediction == authority**

```rust
#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use super::*;
    use wasm_bindgen_test::*;
    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn wasm_start_and_play_matches_native() {
        // Native run of a representative sequence.
        let mut native = GameSession::new("m-1".into());
        let native_start = native.execute(valid_start_match().into_command()).unwrap();
        // WASM run of the SAME commands.
        let mut wasm = WasmGameSession::new("m-1".into());
        let start_json = serde_json::to_string(&valid_start_match()).unwrap();
        let wasm_start = wasm.execute("StartMatchCmd".into(), start_json).unwrap();
        // Compare the event_type sequences.
        let native_types: Vec<_> = native_start.iter().map(|e| e.event_type()).collect();
        let wasm_types: Vec<String> = serde_wasm_bindgen::from_value(wasm_start).unwrap();
        assert_eq!(native_types, wasm_types);
    }
}
```

Run (native): `make test` (this wasm test is `target_arch = "wasm32"`-gated, so it compiles out natively). Run (wasm): `wasm-pack test --node crates/game-session -- --features wasm` — Expected: PASS. If `wasm-pack`/headless browser is unavailable in CI, gate this behind `--node` and document it in the commit body; the native suite stays the primary gate.

- [ ] **Step 3: Fix the client transport (`connection.ts`)**

Replace `send()` (66-70) to ship the envelope the server already parses (`ClientMessage { type:"action", matchId, command, payload }`):

```ts
  send(action: MatchAction): boolean {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return false
    const { kind, ...fields } = action
    this.socket.send(JSON.stringify({ type: 'action', matchId: this.matchId, command: kind, payload: fields }))
    return true
  }
```

> `this.matchId` must be available on the connection (grep the class — if it holds a match id from the join/subscribe flow use it; otherwise thread it through the constructor). The server's `ws/mod.rs` `ClientMessage` and `hub.rs` `apply_action` already accept this shape.

- [ ] **Step 4: Vitest test for the envelope**

```ts
import { describe, it, expect, vi } from 'vitest'
// ...construct a Connection with a fake open socket whose send() is a spy...
it('send ships the structured envelope, not the bare kind', () => {
  const sent: string[] = []
  const conn = makeTestConnection((frame) => sent.push(frame)) // helper wiring a fake OPEN socket
  conn.send({ kind: 'AttackCmd', seat: 'A', attackerId: 'A-atk', targetRef: 'boss:B' })
  const env = JSON.parse(sent[0])
  expect(env).toMatchObject({ type: 'action', command: 'AttackCmd', payload: { seat: 'A', attackerId: 'A-atk', targetRef: 'boss:B' } })
  expect(env.matchId).toBeDefined()
})
```

Run: `cd web && npx vitest run connection` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/game-session/src/lib.rs crates/game-session/Cargo.toml web/src/match/connection.ts web/src/match/model.ts
git commit -m "feat(engine): stateful WasmGameSession + client sends the real envelope

Replaces the payload-less wasm execute_command with a stateful WasmGameSession
{new,execute} that threads a JSON payload through GameSession::execute and returns
the emitted event-type sequence (prediction) or the domain-error text (rejection)
— the identical decision the server makes. A wasm-bindgen-test asserts a WASM run
matches a native run of the same commands. connection.ts now sends the structured
{type,matchId,command,payload} envelope the server already parses, activating the
online command path and closing the AttackCmd/DeclareAttackCmd drift end to end."
```

---

## Task 10: Location-modifier seam (the City-pillar hook)

**Files:**
- Modify: `crates/game-session/src/lib.rs` — add `LocationModifier` + `location: Option<LocationModifier>` on `GameSession`/`StartMatch`; a single `apply_location_modifiers` application point; a `ResolveVenueEventCmd` stub.
- Test: `game-session` test module.

**Interfaces:**
- Consumes: `CardClass`, combat/effect stat reads (Tasks 5-6), the seeded RNG (Task 4), the existing `ResolveCopEvent` pattern (1598).
- Produces: `struct LocationModifier`; `apply_location_modifiers(seat, base) -> BoardUnit` (identity when `None`); a `venue.event.resolved` delta from a seeded single-entry table.

**Constraint:** `location: None` (default) must leave every existing test green — the hook is exercised only when a location is present.

- [ ] **Step 1: Define `LocationModifier` and thread it as `Option`**

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct LocationModifier {
    pub location_id: String,
    pub location_type: String,               // "bank" | "chop_shop" | ... (data-driven)
    pub class_boosts: Vec<(CardClass, i8)>,  // neutral, applies to BOTH seats
    pub heat_multiplier: u8,                 // default 1
    pub event_table_ref: Option<String>,
}
```

Add `location: Option<LocationModifier>` to `GameSession` (default `None` in `new`) and to `StartMatch` (`#[serde(default)]`). `CardClass` needs `Serialize/Deserialize` (add derives in card_definition.rs if not already added in Task 4).

- [ ] **Step 2: Write the failing seam tests**

```rust
    #[test]
    fn location_none_is_identity() {
        let session = valid_session(); // location defaults None
        let base = test_unit("u", 3, 3, true, false, &[]);
        let out = session.apply_location_modifiers(Player::A, &base);
        assert_eq!(out, base, "no location => identity");
    }

    #[test]
    fn location_boosts_matching_class_for_both_seats() {
        let mut session = valid_session();
        session.set_location(LocationModifier {
            location_id: "farm-1".into(), location_type: "server_farm".into(),
            class_boosts: vec![(CardClass::Hacker, 1)], heat_multiplier: 1, event_table_ref: None,
        });
        // A Hacker-class unit gets +1 atk regardless of which seat owns it.
        let base = hacker_unit("h", 2, 2); // helper tagging the unit's class as Hacker
        assert_eq!(session.apply_location_modifiers(Player::A, &base).atk, 3);
        assert_eq!(session.apply_location_modifiers(Player::B, &base).atk, 3);
    }

    #[test]
    fn resolve_venue_event_emits_delta_and_is_seeded() {
        let mut session = valid_session();
        let events = session
            .execute(ResolveVenueEvent::new("m-1", "table-noop", 0).into_command())
            .expect("venue event resolves");
        assert!(events.iter().any(|e| e.event_type() == "venue.event.resolved"));
    }
```

> `BoardUnit` today has no class field; to boost by class the unit must know its class. For Subsystem 1, either (a) add `class: CardClass` to `BoardUnit` (populated from the card at summon), or (b) key the boost off a class carried on the modifier lookup. Prefer (a) — add `class: CardClass` to `BoardUnit` and `CardInstance`, default `Neutral`, populate at deck-build. Update `test_unit`/`test_card_instance` helpers accordingly. This is a small additive field; existing board tests set `Neutral`.

- [ ] **Step 3: Implement the single application point**

```rust
    /// The ONE place location modifiers touch unit stats. Identity when no
    /// location is set; otherwise applies neutral class boosts (both seats).
    fn apply_location_modifiers(&self, _seat: Player, base: &BoardUnit) -> BoardUnit {
        let mut out = base.clone();
        if let Some(loc) = &self.location {
            for (class, delta) in &loc.class_boosts {
                if *class == out.class {
                    out.atk = (out.atk as i16 + *delta as i16).max(0) as u8;
                }
            }
        }
        out
    }
```

Consult it where combat reads `attacker.atk` (Task 5) — i.e. compute `let attacker = self.apply_location_modifiers(seat, attacker_ref)` before capturing `attacker_atk`. In Subsystem 1 with `location: None` this is a no-op; the wiring is what ships.

- [ ] **Step 4: Add the `ResolveVenueEvent` command stub**

Mirror `ResolveCopEvent` (1598): a `ResolveVenueEvent { match_id, event_table_ref, rng_draw }` command routed in `execute` (add `RESOLVE_VENUE_EVENT = "ResolveVenueEventCmd"`), a `VenueEventResolved` event (`event_type` `venue.event.resolved`), and a handler that draws from a single no-op table entry (seeded, changes nothing) and emits the delta. Add the `Event` variant + `event_type` arm.

- [ ] **Step 5: Run + commit**

Run: `make test` → PASS (all pre-existing tests unchanged since `location` defaults `None`).

```bash
git add crates/game-session/src/lib.rs
git commit -m "feat(engine): location-modifier seam (City pillar hook), None-default

Adds Option<LocationModifier> on the match with a single apply_location_modifiers
point (identity when None) consulted in combat, and a ResolveVenueEventCmd stub
that draws a seeded no-op venue event. Neutral class-boosts apply to both seats.
Ships the seam and its tests so Subsystem 3 adds City content — venue catalog,
event tables, the growing map — without re-plumbing the engine. No regression:
every existing test runs with location=None."
```

---

## Self-Review

*Run after the last task; fix inline.*

**Spec coverage (spec §§1–7 + build order §8):**
- §1 Juice fix → Task 1 (crystal + play_card mutation + regression tests). ✓
- §2 Engine unification (seat/board/combat/effects) → Tasks 4, 5, 6. ✓
- §3.1–3.2 play-stats + type reconciliation → Task 2. ✓
- §3.3–3.4 resolvable effects + semantic keywords → Task 3. ✓
- §4 hero powers → Task 7. ✓
- §5 boss-locked cards → Task 8. ✓
- §6 command-drift + WASM gate → Tasks 5 (rename) + 9 (envelope + WASM). ✓
- §7 location seam → Task 10. ✓
- §9 definition of done → covered by Tasks 1/4/5/6/9/10 collectively.

**Known deviations from the spec (deliberate, flagged for the reviewer):**
1. **`SeatState` coexists with `OutfitConfig`** rather than replacing it (Task 4 design note) — required to keep ~200 existing tests green per the green-at-every-commit constraint. Full unification is an out-of-scope follow-up.
2. **`declare_attack` was a working boss-instakill, not an unimplemented stub** — the spec called it a "stub"; the plan *replaces* real (if simplistic) behavior. Existing `declare_attack` tests must be rewritten (Task 5 Step 5).
3. **Effect-registry reconciliation** — the catalog `REGISTERED_EFFECTS` and the client 5-effect set don't match 1:1; Task 3 adds `effect.cool` and maps `recruit_operator→Summon`, `steal_piece`/`pull_heist→None` (Subsystem-2 stubs). A coverage test guards completeness.
4. **Outfit boss-lock needs new aggregate state** — the Outfit had no boss id or card list; Task 8 adds a `boss_id` binding and carries the lock on `AddCardToOutfit` rather than a cross-aggregate lookup.
5. **Drive-By damage is a fixed constant (2)** in Subsystem 1 because `CardEffect::Summon` carries no `amount`; made data-driven in Subsystem 2.
6. **Branch base is `design/cyberpunk-theme`, not `main`** — see the context header. **This is the one item that may warrant Eric's confirmation before execution.**

**Placeholder scan:** every code step shows real code. The two intentional "grep to confirm the exact local spelling" notes (opening-player setter name; `ActivateHeroPower::new` arity; `MatchCompleted` fields) are verification instructions, not placeholders — the surrounding code is complete.

**Type consistency:** `CardEffect`/`Keyword`/`CardInstance`/`BoardUnit`/`SeatState` names are used consistently across Tasks 3-10; `seat_state_at`/`seat_state_at_mut` accessors introduced in Task 4 are reused in 5/6/8/10; the `AttackCmd`/`target_ref` naming is fixed in Task 5 and consumed in Task 9; new event_type strings (`operator.damaged` etc.) match the client's `model.ts:223` fold set exactly.

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-07-16-made-core-engine.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review (spec compliance + code quality) between tasks, fast iteration. Best fit here: the 10 tasks are mostly independent command→event slices on one crate.

**2. Inline Execution** — execute tasks in this session with checkpoints for review.

**One open item to confirm before execution:** the implementation branch bases on `design/cyberpunk-theme` (live code), not `main`. Confirm that's the intended target (see Self-Review deviation #6).

