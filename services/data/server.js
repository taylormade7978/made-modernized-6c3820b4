// MADE async data service.
//
// One process, three surfaces — the CQRS-ish split the app is built around:
//   • GraphQL queries (HTTP POST /graphql)          — reads, incl. searchable/
//     filterable ones (card catalog search, collection, leaderboard, shop).
//   • GraphQL subscriptions (WebSocket /graphql)    — near-real-time push; the
//     server emits on change, the client never polls.
//   • REST mutations (POST /v1/...)                 — commands. A mutation
//     updates in-memory state and PUBLISHES to the matching subscription so
//     every subscribed client updates within a tick.
//
// Seeded with sample Neon-Heist data (design/demo). The real domain data lands
// with the frontend/backend contract reconciliation (vforce360 #1508).

import { createServer } from 'node:http'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { createYoga, createSchema } from 'graphql-yoga'
import { useServer } from 'graphql-ws/lib/use/ws'
import { WebSocketServer } from 'ws'
import { createPubSub } from 'graphql-yoga'

const pubSub = createPubSub()
const __dir = dirname(fileURLToPath(import.meta.url))
// Real MADE card catalog (names, cost, type, class, text, heat, art) parsed
// from the game's own data; see cards.json.
const CATALOG = JSON.parse(readFileSync(join(__dir, 'cards.json'), 'utf8'))

// ── Seed state (mutable; mutations edit it and publish) ───────────────────────
const CARDS = CATALOG.cards
const COLLECTIONS = { guest: CATALOG.collection }

const LEADERBOARD = {
  seasonId: 'S1', page: 1, pageSize: 10, total: 8,
  entries: [
    { rank: 1, playerId: 'p1', displayName: 'NightRunner', rating: 2480, stars: 42 },
    { rank: 2, playerId: 'p2', displayName: 'V0LT', rating: 2390, stars: 39 },
    { rank: 3, playerId: 'p3', displayName: 'CipherJack', rating: 2310, stars: 36 },
    { rank: 4, playerId: 'p4', displayName: 'NeonFox', rating: 2205, stars: 33 },
    { rank: 5, playerId: 'guest', displayName: 'Guest Operator', rating: 2140, stars: 30 },
    { rank: 6, playerId: 'p6', displayName: 'Sable', rating: 2050, stars: 27 },
    { rank: 7, playerId: 'p7', displayName: 'Rook', rating: 1990, stars: 25 },
    { rank: 8, playerId: 'p8', displayName: 'Static', rating: 1910, stars: 22 },
  ],
}

const SHOP_ITEMS = [
  { sku: 'pack.heist', name: 'Neon Heist Pack', description: '5 cards, guaranteed rare+', kind: 'pack', settlement: 'fiat', priceMinor: 199, currency: 'USD' },
  { sku: 'pack.mega', name: 'Mega Score Bundle', description: '20 cards + 1 legendary', kind: 'pack', settlement: 'fiat', priceMinor: 999, currency: 'USD' },
  { sku: 'bp.season1', name: 'Season 1: Neon Heist', description: 'Battle pass — 50 tiers of rewards', kind: 'battlePass', settlement: 'fiat', priceMinor: 1499, currency: 'USD' },
  { sku: 'skin.chrome', name: 'Chrome Ghost (skin)', description: 'Cosmetic skin for Ghost Hacker', kind: 'cosmetic', settlement: 'token', priceMinor: 500, currency: 'MADE' },
  { sku: 'exp.heist', name: 'Neon Heist (expansion)', description: 'Unlock the full Neon Heist set', kind: 'expansion', settlement: 'fiat', priceMinor: 2999, currency: 'USD' },
]

// ── Story campaign: one mission per Crown City boss, escalating tiers ─────────
// A Mission joins a boss + its AI profile. The first three tiers are unlocked;
// later crews unlock as the campaign is cleared (unlocked flag). Launching a
// mission (POST attempts) returns a MissionAttempt whose matchTicket seats the
// player against the AI opponent.
function mission(id, name, desc, tier, bossId, bossName, power, strategy, unlocked) {
  return {
    missionId: id, name, description: desc, difficultyTier: tier,
    boss: { bossId, name: bossName, startingHp: 30, heroPower: power, trademark: power, signatureCardIds: [] },
    aiProfile: { profileId: `ai.${bossId}`, difficultyTier: tier, strategyKind: strategy, mctsBudget: strategy === 'Mcts' ? 4000 : 0 },
    firstClearRewardClaimed: false, unlocked,
  }
}
const MISSIONS = [
  mission('m1', 'First Blood', 'Cain Akaw is looking for a face to put his rage on. Don’t let it be yours.', 'Prologue', 'cain', 'Cain Akaw', 'Mark', 'Scripted', true),
  mission('m2', 'Two-Day Delivery', 'Solomon Vault settles disputes by splitting the shipment. Take the whole thing.', 'Standard', 'solomon', 'Solomon Vault', 'Pull Order', 'Scripted', true),
  mission('m3', 'Royal Court', 'Cleo Reign runs her court like a cartel. Break the entourage, break the throne.', 'Standard', 'cleo', 'Cleo Reign', 'Royal Court', 'Mcts', true),
  mission('m4', 'Shipped Late', 'Nimrod II stacks machinery until you can’t punch through. Punch through.', 'Standard', 'nimrod', 'Nimrod II', 'Tower', 'Mcts', false),
  mission('m5', 'The Demo', 'Moshe Stone will unveil the one play that leaves you holding the bill. Refuse it.', 'Brutal', 'moshe', 'Moshe Stone', 'The Tablet', 'Mcts', false),
  mission('m6', 'Smile for the Camera', 'Lady Homestead is beloved on broadcast, lethal in the alley. Get her off-camera.', 'Brutal', 'homestead', 'Lady Homestead', 'Smile for the Camera', 'Mcts', false),
  mission('m7', 'First Contact', 'Ambassador Zhrrx knows your hand before you draw it. Change the game.', 'Brutal', 'zhrrx', 'Ambassador Zhrrx', 'First Contact', 'Mcts', false),
  mission('m8', 'Thirty Pieces', 'Judith Coin sells you the future and is three borders away before the wallets drain.', 'Legendary', 'judas', 'Judith Coin', 'Thirty Pieces', 'Mcts', false),
  mission('m9', 'Hold the Floor', 'Hollis Crowe out-talks you and buries you in noise. Get a word in edgewise.', 'Legendary', 'crowe', 'Hollis Crowe', 'Hold the Floor', 'Mcts', false),
  mission('m10', 'Mirror Match', 'CL-7N plays your own deck back at you. Beat yourself.', 'Legendary', 'clyde', 'CL-7N “Clyde”', 'Mirror Match', 'Mcts', false),
]

// ── Schema: queries (searchable) + subscriptions (push) ───────────────────────
const typeDefs = /* GraphQL */ `
  type Card { cardId: ID!, name: String!, cost: Int!, cardClass: String!, cardType: String!, rarity: String!, keywords: [String!]!, effectScriptRef: String!, copyCap: Int!, art: String, text: String, heat: Int, atk: Int, hp: Int, artTint: Int }
  type OwnedCard { cardId: ID!, quantity: Int!, cosmeticSkinRef: String }
  type Deck { deckId: ID!, name: String!, cardIds: [String!]!, active: Boolean! }
  type Collection { playerId: ID!, ownedCards: [OwnedCard!]!, decks: [Deck!]! }
  type LeaderboardEntry { rank: Int!, playerId: ID!, displayName: String!, rating: Int!, stars: Int! }
  type LeaderboardPage { seasonId: ID!, entries: [LeaderboardEntry!]!, total: Int!, page: Int!, pageSize: Int! }
  type ShopItem { sku: ID!, name: String!, description: String!, kind: String!, settlement: String!, priceMinor: Int!, currency: String! }

  type Query {
    # Searchable catalog: filter by text, class, type, rarity, cost.
    cards(search: String, cardClass: String, cardType: String, rarity: String, maxCost: Int): [Card!]!
    card(cardId: ID!): Card
    collection(playerId: ID!): Collection
    leaderboard(page: Int, pageSize: Int): LeaderboardPage!
    shopItems: [ShopItem!]!
  }

  type Subscription {
    # Near-real-time push: fires the current value on subscribe, then on change.
    collectionChanged(playerId: ID!): Collection!
    leaderboardChanged: LeaderboardPage!
  }
`

function filterCards(args) {
  const s = (args.search || '').toLowerCase()
  return CARDS.filter((c) =>
    (!s || c.name.toLowerCase().includes(s) || c.keywords.some((k) => k.toLowerCase().includes(s))) &&
    (!args.cardClass || c.cardClass === args.cardClass) &&
    (!args.cardType || c.cardType === args.cardType) &&
    (!args.rarity || c.rarity === args.rarity) &&
    (args.maxCost == null || c.cost <= args.maxCost),
  )
}

const resolvers = {
  Query: {
    cards: (_, args) => filterCards(args),
    card: (_, { cardId }) => CARDS.find((c) => c.cardId === cardId) || null,
    collection: (_, { playerId }) => COLLECTIONS[playerId] || { playerId, ownedCards: [], decks: [] },
    leaderboard: () => LEADERBOARD,
    shopItems: () => SHOP_ITEMS,
  },
  Subscription: {
    collectionChanged: {
      subscribe: (_, { playerId }) => pubSub.subscribe(`collection:${playerId}`),
      resolve: (payload) => payload,
    },
    leaderboardChanged: {
      subscribe: () => pubSub.subscribe('leaderboard'),
      resolve: (payload) => payload,
    },
  },
}

const schema = createSchema({ typeDefs, resolvers })

// Prime a subscriber with the current snapshot immediately on subscribe.
function publishCollection(playerId) { pubSub.publish(`collection:${playerId}`, COLLECTIONS[playerId]) }

const yoga = createYoga({
  schema,
  graphqlEndpoint: '/graphql',
  cors: { origin: ['https://dev.made.vforce360.ai'], credentials: true },
  landingPage: false,
})

// ── HTTP server: GraphQL + REST mutations + health ────────────────────────────
const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x')
  // CORS for the app origin (credentials).
  res.setHeader('Access-Control-Allow-Origin', 'https://dev.made.vforce360.ai')
  res.setHeader('Access-Control-Allow-Credentials', 'true')
  res.setHeader('Access-Control-Allow-Methods', 'GET,POST,PUT,OPTIONS')
  res.setHeader('Access-Control-Allow-Headers', 'Content-Type,Accept')
  if (req.method === 'OPTIONS') { res.writeHead(204); return res.end() }
  if (url.pathname === '/health') { res.writeHead(200); return res.end('ok\n') }

  // REST reads (kept until the frontend fully moves to GraphQL queries). These
  // mirror the GraphQL Query fields so the current REST client keeps working.
  const rjson = (v) => { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(v)) }
  if (req.method === 'GET') {
    if (url.pathname === '/v1/catalog/cards') return rjson(CARDS)
    if (/^\/v1\/catalog\/cards\/.+/.test(url.pathname)) return rjson(CARDS.find((c) => c.cardId === decodeURIComponent(url.pathname.split('/').pop())) || null)
    if (url.pathname === '/v1/catalog/expansions') return rjson([{ setCode: 'HEIST', name: 'Neon Heist', releaseChannel: 'live', cardIds: CARDS.map((c) => c.cardId) }])
    const col = url.pathname.match(/^\/v1\/collection\/([^/]+)$/)
    if (col) return rjson(COLLECTIONS[col[1]] || { playerId: col[1], ownedCards: [], decks: [] })
    if (url.pathname === '/v1/leaderboard') return rjson(LEADERBOARD)
    if (url.pathname === '/v1/shop/items') return rjson(SHOP_ITEMS)
    const story = url.pathname.match(/^\/v1\/story\/([^/]+)\/missions$/)
    if (story) return rjson({ playerId: story[1], missions: MISSIONS })
    if (url.pathname.startsWith('/v1/')) return rjson([])
  }

  // REST mutations. Each edits state and PUBLISHES so subscribers update live.
  if (req.method === 'PUT' && /^\/v1\/collection\/[^/]+\/decks\/[^/]+$/.test(url.pathname)) {
    const [, , , playerId, , deckId] = url.pathname.split('/')
    const body = await readJson(req)
    const col = COLLECTIONS[playerId] || (COLLECTIONS[playerId] = { playerId, ownedCards: [], decks: [] })
    const deck = { deckId, name: body.name ?? deckId, cardIds: body.cardIds ?? [], active: !!body.active }
    const i = col.decks.findIndex((d) => d.deckId === deckId)
    if (i >= 0) col.decks[i] = deck; else col.decks.push(deck)
    if (deck.active) col.decks.forEach((d) => { if (d.deckId !== deckId) d.active = false })
    publishCollection(playerId) // ← live push, no polling
    res.writeHead(200, { 'Content-Type': 'application/json' })
    return res.end(JSON.stringify(deck))
  }
  // Launch a story mission → a MissionAttempt with a match ticket the board joins.
  const att = req.method === 'POST' && url.pathname.match(/^\/v1\/story\/([^/]+)\/missions\/([^/]+)\/attempts$/)
  if (att) {
    const [, playerId, missionId] = att
    const m = MISSIONS.find((x) => x.missionId === missionId)
    const order = {
      attemptId: 'att.' + Math.abs(hash(playerId + missionId + Date.now())).toString(36),
      missionId, playerId, difficultyTier: m ? m.difficultyTier : 'Standard',
      matchTicket: `mission:${missionId}:${playerId}`, scriptedStateStep: 0, missionCompleted: false,
    }
    res.writeHead(200, { 'Content-Type': 'application/json' })
    return res.end(JSON.stringify(order))
  }
  if (req.method === 'POST' && url.pathname === '/v1/shop/orders') {
    const body = await readJson(req)
    const order = { orderId: 'ord.' + Math.abs(hash(JSON.stringify(body) + Date.now())).toString(36), status: 'created', sku: body?.sku ?? null }
    res.writeHead(200, { 'Content-Type': 'application/json' })
    return res.end(JSON.stringify(order))
  }

  return yoga(req, res)
})

// ── WebSocket: GraphQL subscriptions (canonical yoga + graphql-ws wiring) ─────
const wss = new WebSocketServer({ server, path: '/graphql' })
useServer(
  {
    execute: (args) => args.rootValue.execute(args),
    subscribe: (args) => args.rootValue.subscribe(args),
    onSubscribe: async (ctx, msg) => {
      const { schema, execute, subscribe, contextFactory, parse, validate } =
        yoga.getEnveloped({ ...ctx, req: ctx.extra.request, socket: ctx.extra.socket, params: msg.payload })
      const args = {
        schema,
        operationName: msg.payload.operationName,
        document: parse(msg.payload.query),
        variableValues: msg.payload.variables,
        contextValue: await contextFactory(),
        rootValue: { execute, subscribe },
      }
      const errors = validate(args.schema, args.document)
      if (errors.length) return errors
      return args
    },
  },
  wss,
)

function readJson(req) {
  return new Promise((resolve) => { let b = ''; req.on('data', (d) => (b += d)); req.on('end', () => { try { resolve(JSON.parse(b || '{}')) } catch { resolve({}) } }) })
}
function hash(s) { let h = 0; for (let i = 0; i < s.length; i++) { h = (h << 5) - h + s.charCodeAt(i); h |= 0 } return h }

const PORT = process.env.PORT || 8080
server.listen(PORT, '0.0.0.0', () => console.log(`MADE data service on :${PORT} (GraphQL /graphql, subscriptions ws /graphql, REST /v1 mutations)`))
