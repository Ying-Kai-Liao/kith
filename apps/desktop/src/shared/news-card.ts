// The News card (DESIGN.md "News"): a scheduled run's brief as rows, each a title, one line and "source · domain" that
// opens its link. The host found the rows in the text the run sent; the app adds only the fixed words.

import type { RichCard } from './rich-card.ts'
import { daysFrom, quietEvery } from './schedule-card.ts'

export interface NewsRow { title: string; line: string; source: string | null; domain: string; url: string }

export interface NewsView {
  /** "Morning brief · Sat 25 Oct". */
  title: string
  /** "every day 7:00", or null for a job the host couldn't name the schedule of. */
  every: string | null
  lead: string
  tail: string
  rows: NewsRow[]
  /** The rows shown before "Show N more". */
  shown: NewsRow[]
  more: number
  /** After its day: "Morning brief · Fri · 6 items"; tap opens it. */
  fold: string | null
}

/** Three rows show; the rest sit behind "Show N more". */
export const NEWS_ROWS_SHOWN = 3

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']
const str = (v: unknown): string => (typeof v === 'string' ? v : '')

/** A row the app may open: an http(s) link with a title. */
function rowOf(raw: unknown): NewsRow | null {
  if (!raw || typeof raw !== 'object') return null
  const r = raw as Record<string, unknown>
  const url = str(r.url)
  if (!str(r.title) || !/^https?:\/\//i.test(url)) return null
  return { title: str(r.title), line: str(r.line), source: str(r.source) || null, domain: str(r.domain), url }
}

export function newsView(card: RichCard, opts: { postedAt: number; now: number; expanded?: boolean; opened?: boolean }): NewsView {
  const s = card.snapshot ?? {}
  const rows = (Array.isArray(s.rows) ? s.rows : []).map(rowOf).filter((r): r is NewsRow => r !== null)
  const d = new Date(opts.postedAt)
  const name = str(s.name) || 'News'
  // A brief opened again after its day shows every item, with no "Show more".
  const all = opts.expanded || opts.opened || rows.length <= NEWS_ROWS_SHOWN
  const shown = all ? rows : rows.slice(0, NEWS_ROWS_SHOWN)
  const past = daysFrom(opts.postedAt, opts.now) < 0
  return {
    title: `${name} · ${DAYS[d.getDay()]} ${d.getDate()} ${MONTHS[d.getMonth()]}`,
    every: str(s.every) ? quietEvery(str(s.every)) : null,
    lead: str(s.lead),
    tail: str(s.tail),
    rows,
    shown,
    more: rows.length - shown.length,
    fold: past && !opts.opened ? `${name} · ${DAYS[d.getDay()]} · ${rows.length} ${rows.length === 1 ? 'item' : 'items'}` : null,
  }
}
