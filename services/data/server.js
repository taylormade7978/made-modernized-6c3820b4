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
import { createYoga, createSchema } from 'graphql-yoga'
import { useServer } from 'graphql-ws/lib/use/ws'
import { WebSocketServer } from 'ws'
import { createPubSub } from 'graphql-yoga'

const pubSub = createPubSub()

// ── Seed state (mutable; mutations edit it and publish) ───────────────────────
const CARDS = [
  { cardId: 'card.getaway_driver', name: 'Getaway Driver', cost: 3, cardClass: 'tempo', cardType: 'unit', rarity: 'common', keywords: ['Recruit', 'Wheels'], effectScriptRef: 'fx.recruit', copyCap: 3 },
  { cardId: 'card.safecracker', name: 'Safecracker', cost: 2, cardClass: 'combo', cardType: 'unit', rarity: 'uncommon', keywords: ['Heist'], effectScriptRef: 'fx.crack', copyCap: 3 },
  { cardId: 'card.muscle', name: 'The Muscle', cost: 4, cardClass: 'aggression', cardType: 'unit', rarity: 'common', keywords: ['Bruiser'], effectScriptRef: 'fx.slam', copyCap: 3 },
  { cardId: 'card.hacker', name: 'Ghost Hacker', cost: 3, cardClass: 'control', cardType: 'unit', rarity: 'rare', keywords: ['Intrusion', 'Silence'], effectScriptRef: 'fx.hack', copyCap: 2 },
  { cardId: 'card.smoke_bomb', name: 'Smoke Bomb', cost: 1, cardClass: 'tempo', cardType: 'spell', rarity: 'common', keywords: ['Evasion'], effectScriptRef: 'fx.smoke', copyCap: 3 },
  { cardId: 'card.tripwire', name: 'Tripwire', cost: 2, cardClass: 'control', cardType: 'trap', rarity: 'uncommon', keywords: ['Ambush'], effectScriptRef: 'fx.snare', copyCap: 3 },
  { cardId: 'card.armored_van', name: 'Armored Van', cost: 5, cardClass: 'control', cardType: 'unit', rarity: 'epic', keywords: ['Vehicle', 'Shield'], effectScriptRef: 'fx.armor', copyCap: 2 },
  { cardId: 'card.kingpin', name: 'The Kingpin', cost: 7, cardClass: 'aggression', cardType: 'leader', rarity: 'legendary', keywords: ['Boss', 'Command'], effectScriptRef: 'fx.command', copyCap: 1 },
  { cardId: 'card.wildcard', name: 'Wildcard', cost: 0, cardClass: 'neutral', cardType: 'spell', rarity: 'rare', keywords: ['Draw'], effectScriptRef: 'fx.draw', copyCap: 2 },
  { cardId: 'card.overclock', name: 'Overclock', cost: 2, cardClass: 'combo', cardType: 'spell', rarity: 'epic', keywords: ['Juice', 'Ramp'], effectScriptRef: 'fx.ramp', copyCap: 2 },
]

const COLLECTIONS = {
  guest: {
    playerId: 'guest',
    ownedCards: [
      { cardId: 'card.getaway_driver', quantity: 3, cosmeticSkinRef: null },
      { cardId: 'card.safecracker', quantity: 2, cosmeticSkinRef: null },
      { cardId: 'card.muscle', quantity: 3, cosmeticSkinRef: null },
      { cardId: 'card.hacker', quantity: 2, cosmeticSkinRef: 'skin.chrome' },
      { cardId: 'card.smoke_bomb', quantity: 3, cosmeticSkinRef: null },
      { cardId: 'card.armored_van', quantity: 1, cosmeticSkinRef: null },
      { cardId: 'card.kingpin', quantity: 1, cosmeticSkinRef: 'skin.gold' },
      { cardId: 'card.overclock', quantity: 2, cosmeticSkinRef: null },
    ],
    decks: [
      { deckId: 'deck.aggro', name: 'Smash & Grab', cardIds: ['card.muscle', 'card.muscle', 'card.getaway_driver', 'card.kingpin', 'card.smoke_bomb'], active: true },
      { deckId: 'deck.control', name: 'Ghost Protocol', cardIds: ['card.hacker', 'card.armored_van', 'card.tripwire', 'card.overclock', 'card.wildcard'], active: false },
    ],
  },
}

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

// ── Story: single-player campaign (missions = boss encounters) ────────────────
// Each mission binds a boss to a difficulty tier + its AI profile. Unlock is a
// linear chain: a mission is playable once its predecessor is cleared. Shapes
// mirror the frontend Mission/Boss/AIProfile (web/src/api/types.ts).
const MISSIONS = [
  {
    missionId: 'mission.prologue', name: 'The Setup',
    description: 'Learn the ropes against a scripted fence in the back alley.',
    difficultyTier: 'Prologue',
    boss: { bossId: 'boss.fence', name: 'The Fence', startingHp: 20, heroPower: 'Appraise', trademark: 'Buys low, sells you out.', signatureCardIds: ['card.smoke_bomb', 'card.tripwire'] },
    aiProfile: { profileId: 'ai.prologue', difficultyTier: 'Prologue', strategyKind: 'Scripted', mctsBudget: 0 },
    firstClearRewardClaimed: true, unlocked: true,
  },
  {
    missionId: 'mission.vault', name: 'Cracking the Vault',
    description: 'Outplay the security chief guarding the downtown vault.',
    difficultyTier: 'Standard',
    boss: { bossId: 'boss.warden', name: 'Warden Kessler', startingHp: 25, heroPower: 'Lockdown', trademark: 'Every turn, a door slams shut.', signatureCardIds: ['card.armored_van', 'card.tripwire', 'card.hacker'] },
    aiProfile: { profileId: 'ai.standard', difficultyTier: 'Standard', strategyKind: 'Mcts', mctsBudget: 800 },
    firstClearRewardClaimed: false, unlocked: true,
  },
  {
    missionId: 'mission.rival', name: 'Double Cross',
    description: 'Your old partner turned rival — no scripts, all pressure.',
    difficultyTier: 'Brutal',
    boss: { bossId: 'boss.rival', name: 'Silas "Ghost" Marrow', startingHp: 28, heroPower: 'Vanish', trademark: 'Slips your best play every time.', signatureCardIds: ['card.hacker', 'card.overclock', 'card.smoke_bomb'] },
    aiProfile: { profileId: 'ai.brutal', difficultyTier: 'Brutal', strategyKind: 'Mcts', mctsBudget: 2400 },
    firstClearRewardClaimed: false, unlocked: false,
  },
  {
    missionId: 'mission.kingpin', name: 'The Kingpin',
    description: 'The final score — beat the boss of bosses at his own table.',
    difficultyTier: 'Legendary',
    boss: { bossId: 'boss.kingpin', name: 'The Kingpin', startingHp: 32, heroPower: 'Command', trademark: 'Commands the board; punishes greed.', signatureCardIds: ['card.kingpin', 'card.muscle', 'card.armored_van'] },
    aiProfile: { profileId: 'ai.legendary', difficultyTier: 'Legendary', strategyKind: 'Mcts', mctsBudget: 6000 },
    firstClearRewardClaimed: false, unlocked: false,
  },
]

// Per-player campaign view. Demo: 'guest' keeps the seeded progress above; any
// other player starts fresh with only the prologue unlocked.
function storyFor(playerId) {
  if (playerId === 'guest') return { playerId, missions: MISSIONS }
  const missions = MISSIONS.map((m, i) => ({ ...m, firstClearRewardClaimed: false, unlocked: i === 0 }))
  return { playerId, missions }
}

// ── Schema: queries (searchable) + subscriptions (push) ───────────────────────
const typeDefs = /* GraphQL */ `
  type Card { cardId: ID!, name: String!, cost: Int!, cardClass: String!, cardType: String!, rarity: String!, keywords: [String!]!, effectScriptRef: String!, copyCap: Int! }
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
    if (story) return rjson(storyFor(decodeURIComponent(story[1])))
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
  // Launch a mission attempt: seats the player vs the mission's AI opponent and
  // hands back a match ticket the realtime WS uses to play it. Mirrors the
  // frontend MissionAttempt (web/src/api/types.ts).
  const attempt = req.method === 'POST' && url.pathname.match(/^\/v1\/story\/([^/]+)\/missions\/([^/]+)\/attempts$/)
  if (attempt) {
    const playerId = decodeURIComponent(attempt[1])
    const missionId = decodeURIComponent(attempt[2])
    const mission = MISSIONS.find((m) => m.missionId === missionId)
    if (!mission) { res.writeHead(404, { 'Content-Type': 'application/json' }); return res.end(JSON.stringify({ error: 'unknown mission', missionId })) }
    const attemptId = 'att.' + Math.abs(hash(`${playerId}:${missionId}:${Date.now()}`)).toString(36)
    const result = {
      attemptId, missionId, playerId,
      difficultyTier: mission.difficultyTier,
      matchTicket: 'ticket.' + Math.abs(hash(attemptId)).toString(36),
      scriptedStateStep: 0,
      missionCompleted: false,
    }
    res.writeHead(201, { 'Content-Type': 'application/json' })
    return res.end(JSON.stringify(result))
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
