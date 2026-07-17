#!/usr/bin/env node
// Content / balance validator for the MADE card catalog.
//
// Enforces the fair-play rules the game is designed around (see the in-app
// Rules screen): sane cost curve, copy caps, unique ids, art present, and — the
// balance guardrails — no undercosted stat outliers and comparable starter
// decks. EVERY threshold comes from balance.json so fair-play can be retuned
// when an exploit emerges, with no code change. Exits non-zero on any ERROR so
// it can gate CI; WARN is advisory.
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const __dir = dirname(fileURLToPath(import.meta.url))
const { cards, collection } = JSON.parse(readFileSync(join(__dir, 'cards.json'), 'utf8'))
const B = JSON.parse(readFileSync(join(__dir, 'balance.json'), 'utf8'))
const byId = new Map(cards.map((c) => [c.cardId, c]))
const errors = []
const warns = []
const E = (m) => errors.push(m)
const W = (m) => warns.push(m)

// ── Structural integrity ──────────────────────────────────────────────────────
const seen = new Set()
for (const c of cards) {
  if (seen.has(c.cardId)) E(`duplicate cardId: ${c.cardId}`)
  seen.add(c.cardId)
  if (typeof c.cost !== 'number' || c.cost < B.cost.min || c.cost > B.cost.max)
    E(`${c.cardId}: cost ${c.cost} out of [${B.cost.min},${B.cost.max}]`)
  if (!c.art) E(`${c.cardId}: missing art`)
  const cap = c.rarity === 'legendary' ? B.deck.copyCapLegendary : B.deck.copyCapDefault
  if (c.copyCap > cap) E(`${c.cardId}: copyCap ${c.copyCap} exceeds ${cap} for ${c.rarity}`)
}

// ── Balance: no undercosted stat outliers (configurable budget) ───────────────
const { slope, base, appliesTo } = B.statBudget
for (const c of cards) {
  if (appliesTo.includes(c.cardType) && c.atk != null && c.hp != null) {
    const budget = slope * c.cost + base
    if (c.atk + c.hp > budget)
      W(`${c.cardId}: stat outlier (${c.atk}+${c.hp}=${c.atk + c.hp} > budget ${budget} at cost ${c.cost})`)
  }
}

// ── Balance: answer density — every threat class has removal ───────────────────
if (B.answerDensity.requireDirectDamage) {
  const hasDamageJob = cards.some((c) => c.cardType === 'Job' && /damage|deal \d/i.test(c.text || ''))
  if (!hasDamageJob) E('no direct-damage removal exists — big Operators would be unanswerable')
}
if (B.answerDensity.requireTempoRemoval) {
  const hasLock = cards.some((c) => /cuff|lock down|stun|can't attack/i.test(c.text || ''))
  if (!hasLock) E('no tempo removal (cuff/stun/lock) exists')
}

// ── Balance: starter-deck parity ──────────────────────────────────────────────
const decks = collection?.decks ?? []
const avgCost = (d) => {
  const cs = d.cardIds.map((id) => byId.get(id)).filter(Boolean)
  return cs.length ? cs.reduce((s, c) => s + c.cost, 0) / cs.length : 0
}
for (const d of decks) {
  if (d.cardIds.length !== B.deck.size) E(`starter deck "${d.name}": ${d.cardIds.length} cards (must be ${B.deck.size})`)
  for (const id of d.cardIds) if (!byId.has(id)) E(`starter deck "${d.name}": unknown card ${id}`)
}
if (decks.length >= 2) {
  const costs = decks.map(avgCost)
  const spread = Math.max(...costs) - Math.min(...costs)
  const label = decks.map((d, i) => `${d.name}=${costs[i].toFixed(2)}`).join(', ')
  if (spread > B.starterDecks.avgCostSpreadMax)
    W(`starter-deck avg-cost spread ${spread.toFixed(2)} > ${B.starterDecks.avgCostSpreadMax} (${label}) — may be unbalanced`)
  else console.log(`starter decks balanced: ${label} (spread ${spread.toFixed(2)})`)
}

// ── Report ─────────────────────────────────────────────────────────────────────
console.log(`\nvalidated ${cards.length} cards, ${decks.length} decks against balance.json v${B.version}`)
for (const w of warns) console.log(`  WARN  ${w}`)
for (const e of errors) console.log(`  ERROR ${e}`)
if (errors.length) { console.error(`\n${errors.length} error(s) — content invalid`); process.exit(1) }
console.log(`\nOK (${warns.length} warning(s))`)
