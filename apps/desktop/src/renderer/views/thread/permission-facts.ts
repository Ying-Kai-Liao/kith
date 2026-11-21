import type { CardFact } from '../../../shared/models.ts'

/** A piece of a fact's text, bold or not. */
export interface Piece {
  text: string
  bold: boolean
}

/** The SF Symbol for a fact's icon word; anything the host adds later draws as a plain document. */
export function factSymbol(icon: string): string {
  switch (icon) {
    case 'terminal':
      return 'terminal'
    case 'down':
      return 'arrow.down.circle'
    case 'globe':
      return 'globe'
    case 'pencil':
      return 'pencil'
    case 'trash':
      return 'trash'
    case 'stop':
      return 'stop.circle'
    case 'folder':
      return 'folder'
    case 'eye':
      return 'eye'
    default:
      return 'doc'
  }
}

/** A fact's text cut into plain and bold pieces: each `strong` part is found in order, so a repeated word bolds the right one. */
export function factPieces(fact: Pick<CardFact, 'text' | 'strong'>): Piece[] {
  const out: Piece[] = []
  let at = 0
  for (const s of fact.strong ?? []) {
    if (!s) continue
    const i = fact.text.indexOf(s, at)
    if (i < 0) continue
    if (i > at) out.push({ text: fact.text.slice(at, i), bold: false })
    out.push({ text: s, bold: true })
    at = i + s.length
  }
  if (at < fact.text.length) out.push({ text: fact.text.slice(at), bold: false })
  return out
}

/** The muted words after a fact: the host's label, then this Mac for the line that says where it runs. */
export function factNote(fact: Pick<CardFact, 'muted' | 'place'>, hostName: string): string {
  return [fact.muted, fact.place ? hostName : undefined].filter(Boolean).join(' · ')
}

/** The line above the intent: "Runs on <Mac> …" stays unless a fact already says where (its `place`), then only who asked. */
export function placeLine(facts: Pick<CardFact, 'place'>[], runsOn: string, chain?: string): string | undefined {
  return facts.some((f) => f.place) ? chain : runsOn
}

/**
 * The facts to draw, or null for today's card: a host that sent none, or only malformed ones, shows the old card.
 * A fact missing its text is dropped, so a half-written line never draws.
 */
export function cardFacts(d: { facts?: CardFact[] }): CardFact[] | null {
  const facts = (d.facts ?? []).filter((f) => f && typeof f.text === 'string' && f.text.length > 0)
  return facts.length > 0 ? facts : null
}
