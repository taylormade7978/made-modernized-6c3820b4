/**
 * Request/response DTOs for the `/v1` REST surface.
 *
 * These mirror the backend domain aggregates in `crates/domain` (CardDefinition,
 * PlayerCollection, ExpansionSet, Order, RankedStanding/Season) expressed in
 * idiomatic camelCase TypeScript. Because the REST layer is the contract between
 * the PWA and the edge, these interfaces are the client-side source of truth for
 * those payloads; keep them in step with the domain structs as endpoints land.
 *
 * Enums are modelled as string unions rather than TS `enum`s so they erase at
 * runtime (no emitted objects) and compare directly against JSON string values.
 */

// ── Catalog: card definitions & expansion sets ────────────────────────────────

/** Mirrors `card_definition::CardType`. */
export type CardType = 'unit' | 'spell' | 'trap' | 'leader'
/** Mirrors `card_definition::CardClass`. */
export type CardClass = 'neutral' | 'aggression' | 'control' | 'tempo' | 'combo'
/** Mirrors `card_definition::Rarity`. */
export type Rarity = 'common' | 'uncommon' | 'rare' | 'epic' | 'legendary'

/** A published card definition (mirrors `CardDefined`). */
export interface Card {
  readonly cardId: string
  readonly name: string
  readonly cost: number
  readonly cardClass: CardClass
  readonly cardType: CardType
  readonly rarity: Rarity
  readonly keywords: readonly string[]
  readonly effectScriptRef: string
  /** Max copies of this card allowed in a single deck. */
  readonly copyCap: number
}

/** Release channel of an expansion set (mirrors `ExpansionReleased.release_channel`). */
export type ReleaseChannel = 'alpha' | 'beta' | 'live'

/** A card expansion / set (mirrors `expansion_set` aggregate). */
export interface ExpansionSet {
  readonly setCode: string
  readonly name: string
  readonly releaseChannel: ReleaseChannel
  readonly cardIds: readonly string[]
}

// ── Collection & deck ─────────────────────────────────────────────────────────

/** A single owned card row: the definition plus per-player state. */
export interface OwnedCard {
  readonly cardId: string
  /** How many copies the player owns. */
  readonly quantity: number
  /** Equipped cosmetic skin ref, if any (mirrors `CosmeticEquipped`). */
  readonly cosmeticSkinRef: string | null
}

/** A saved deck of card references. */
export interface Deck {
  readonly deckId: string
  readonly name: string
  /** Ordered card ids composing the deck (duplicates allowed up to copyCap). */
  readonly cardIds: readonly string[]
  /** Whether this is the player's active deck. */
  readonly active: boolean
}

/** Response of `GET /v1/collection/{playerId}`. */
export interface CollectionResponse {
  readonly playerId: string
  readonly ownedCards: readonly OwnedCard[]
  readonly decks: readonly Deck[]
}

/** Body of `PUT /v1/collection/{playerId}/decks/{deckId}`. */
export interface SaveDeckRequest {
  readonly name: string
  readonly cardIds: readonly string[]
  readonly active?: boolean
}

// ── Leaderboard ───────────────────────────────────────────────────────────────

/** One ranked row (mirrors a `RankedStanding` projection / `Season` leaderboard). */
export interface LeaderboardEntry {
  readonly rank: number
  readonly playerId: string
  readonly displayName: string
  readonly rating: number
  readonly stars: number
}

/** Response of `GET /v1/leaderboard` — a page of ranked standings. */
export interface LeaderboardPage {
  readonly seasonId: string
  readonly entries: readonly LeaderboardEntry[]
  /** Total ranked players in the season (for pagination UIs). */
  readonly total: number
  readonly page: number
  readonly pageSize: number
}

/** Query params for `GET /v1/leaderboard`. */
export interface LeaderboardQuery {
  readonly seasonId?: string
  readonly page?: number
  readonly pageSize?: number
}

// ── Shop & orders ─────────────────────────────────────────────────────────────

/**
 * What a shop item unlocks — mirrors the shop-payments aggregate it maps to
 * (`card_pack`, `battle_pass`, cosmetics, `expansion_set`).
 */
export type ShopItemKind = 'pack' | 'battlePass' | 'cosmetic' | 'expansion'

/**
 * How a shop item is paid for.
 *
 *  - `fiat` — settled through Stripe (the only path this client initiates), per
 *    the `order` aggregate's "fiat via Stripe only" invariant.
 *  - `token` — bought with the in-game `$MADE` soft currency. These are NEVER
 *    routed through Stripe and are capability-gated off native-shell builds
 *    (hidden or web-redirected), per the app-store crypto/wallet policies.
 */
export type ShopSettlement = 'fiat' | 'token'

/** A purchasable shop item (pack, battle-pass, cosmetic, expansion). */
export interface ShopItem {
  readonly sku: string
  readonly name: string
  readonly description: string
  readonly kind: ShopItemKind
  /** Fiat items check out via Stripe; token items are gated / web-redirected. */
  readonly settlement: ShopSettlement
  /** Price in minor currency units (e.g. cents), to avoid float money. */
  readonly priceMinor: number
  /** ISO-4217 code for a fiat item, or `MADE` for a `$MADE`-settled token item. */
  readonly currency: string
}

/** Lifecycle state of an order (mirrors the `order` aggregate transitions). */
export type OrderStatus = 'created' | 'paid' | 'fulfilled' | 'refunded'

/** An order and its current state (mirrors `Order`). */
export interface Order {
  readonly orderId: string
  readonly playerId: string
  readonly lineItems: readonly string[]
  readonly currency: string
  readonly status: OrderStatus
  /**
   * Hosted Stripe Checkout Session URL to redirect the buyer to while the order
   * is still `created`. The shop-payments-service opens the Checkout Session as
   * part of `CreateOrderCmd` and returns its URL here; the client redirects the
   * browser to it. Absent / `null` once the order is paid (settled by webhook).
   */
  readonly checkoutUrl?: string | null
}

/** Body of `POST /v1/shop/orders` (mirrors `CreateOrderCmd`). */
export interface CreateOrderRequest {
  readonly playerId: string
  readonly lineItems: readonly string[]
  readonly currency: string
}

// ── Story: missions, bosses & AI opponents ────────────────────────────────────

/** Difficulty tier a mission runs at (mirrors `ai_profile::DifficultyTier`). */
export type DifficultyTier = 'Prologue' | 'Standard' | 'Brutal' | 'Legendary'

/** The strategy that drives the AI opponent (mirrors `ai_profile::StrategyKind`). */
export type StrategyKind = 'Scripted' | 'Mcts'

/** A story boss (mirrors the `boss_definition` aggregate's defined state). */
export interface Boss {
  readonly bossId: string
  readonly name: string
  readonly startingHp: number
  readonly heroPower: string
  readonly trademark: string
  readonly signatureCardIds: readonly string[]
}

/** The AI opponent bound to a mission's difficulty (mirrors `AIProfile`). */
export interface AIProfile {
  readonly profileId: string
  readonly difficultyTier: DifficultyTier
  readonly strategyKind: StrategyKind
  /** MCTS search budget; `0` for the scripted prologue strategy. */
  readonly mctsBudget: number
}

/** A single-player story mission: a boss encounter at a difficulty tier. */
export interface Mission {
  readonly missionId: string
  readonly name: string
  readonly description: string
  readonly difficultyTier: DifficultyTier
  readonly boss: Boss
  readonly aiProfile: AIProfile
  /** Whether this player has already claimed the mission's first-clear reward. */
  readonly firstClearRewardClaimed: boolean
  /** Whether the predecessor mission is cleared, so this one is playable. */
  readonly unlocked: boolean
}

/** Response of `GET /v1/story/{playerId}/missions` — a player's campaign. */
export interface StoryResponse {
  readonly playerId: string
  readonly missions: readonly Mission[]
}

/** Body of `POST /v1/story/{playerId}/missions/{missionId}/attempts`. */
export interface LaunchMissionRequest {
  readonly playerId: string
  readonly missionId: string
}

/** A launched mission attempt vs its AI opponent (mirrors `MissionAttempt`). */
export interface MissionAttempt {
  readonly attemptId: string
  readonly missionId: string
  readonly playerId: string
  readonly difficultyTier: DifficultyTier
  /**
   * The matchmaking ticket that joins the authoritative match the AI-opponent
   * service is seated in. Handed to the realtime WebSocket to play the mission.
   */
  readonly matchTicket: string
  /** The scripted mission-state step this attempt starts at. */
  readonly scriptedStateStep: number
  /** Whether the attempt's outcome has already been resolved. */
  readonly missionCompleted: boolean
}
