// The Result card (DESIGN.md "Reply cards"): what came out of a reply, with its actions. Jev sorted a finished reply and
// picked which of its own lines fill the slots (host `rich_card::jev`); every string is the reply's own, and the app
// adds only the fixed words. The entry's text is the card's twin, shown by "Show as text".
// v2 is the Info card: a title, one sub line and Key: value rows. v1 (a flat facts list) keeps drawing for old entries.

import type { RichCard } from './rich-card.ts'

export interface ResultLink { label: string; url: string }
export interface ResultRow { label: string; value: string }

export interface ResultView {
  title: string
  /** The verdict or summary line under the title (v2). */
  sub: string
  /** Key: value rows (v2); empty for a v1 card. */
  rows: ResultRow[]
  /** Mono facts, drawn joined with " · " (v1); empty for a v2 card. */
  facts: string[]
  /** Link rows (v2) or buttons (v1) that open a link the reply held. */
  links: ResultLink[]
}

const str = (v: unknown): string => (typeof v === 'string' ? v.trim() : '')
const obj = (v: unknown): Record<string, unknown> | null => (v && typeof v === 'object' ? (v as Record<string, unknown>) : null)

/** The card to draw, or null (the plain reply) when it lacks a title or anything to show. */
export function resultView(card: RichCard): ResultView | null {
  const s = card.snapshot ?? {}
  const title = str(s.title)
  const v2 = card.v >= 2
  const links = (Array.isArray(s.links) ? s.links : []).flatMap((raw): ResultLink[] => {
    const l = obj(raw)
    const url = str(l?.url)
    // Only a web link opens from a card. A v2 link row with no label shows the address itself.
    return /^https?:\/\//i.test(url) ? [{ label: str(l?.label) || (v2 ? url.replace(/^https?:\/\//i, '') : 'Open'), url }] : []
  })
  if (v2) {
    // A row with no label draws its value alone; one with no value is dropped.
    const rows = (Array.isArray(s.rows) ? s.rows : []).flatMap((raw): ResultRow[] => {
      const r = obj(raw)
      const value = str(r?.value)
      return value ? [{ label: str(r?.label), value }] : []
    })
    return title && rows.length ? { title, sub: str(s.sub), rows: rows.slice(0, 5), facts: [], links: links.slice(0, 2) } : null
  }
  const facts = (Array.isArray(s.facts) ? s.facts : []).map(str).filter(Boolean)
  return title && facts.length ? { title, sub: '', rows: [], facts: facts.slice(0, 4), links: links.slice(0, 2) } : null
}
