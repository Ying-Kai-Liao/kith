// The Result card (DESIGN.md "Reply cards"): what came out of a reply, with its actions. Jev sorted a finished reply and
// picked which of its own lines fill the slots (host `rich_card::jev`); every string is the reply's own, and the app
// adds only the fixed words. The entry's text is the card's twin, shown by "Show as text".

import type { RichCard } from './rich-card.ts'

export interface ResultLink { label: string; url: string }

export interface ResultView {
  title: string
  /** Mono facts, drawn joined with " · ". */
  facts: string[]
  /** Buttons that open a link the reply held. */
  links: ResultLink[]
}

const str = (v: unknown): string => (typeof v === 'string' ? v.trim() : '')

/** The card to draw, or null (the plain reply) when it lacks a title or a fact. */
export function resultView(card: RichCard): ResultView | null {
  const s = card.snapshot ?? {}
  const title = str(s.title)
  const facts = (Array.isArray(s.facts) ? s.facts : []).map(str).filter(Boolean)
  const links = (Array.isArray(s.links) ? s.links : []).flatMap((raw): ResultLink[] => {
    if (!raw || typeof raw !== 'object') return []
    const url = str((raw as Record<string, unknown>).url)
    // Only a web link opens from a card.
    return /^https?:\/\//i.test(url) ? [{ label: str((raw as Record<string, unknown>).label) || 'Open', url }] : []
  })
  return title && facts.length ? { title, facts: facts.slice(0, 4), links: links.slice(0, 2) } : null
}
