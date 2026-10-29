import type { Entry } from '../../../shared/models.ts'
import { VISIBLE_STEPS } from '../../../shared/thread-feed.ts'
import { groupExchanges, type BotExchange } from './bot-exchange.ts'

export type ChatItem =
  | { id: string; kind: 'separator'; date: number }
  | { id: string; kind: 'entry'; entry: Entry; groupStart: boolean }
  | { id: string; kind: 'exchanges'; entry: Entry; exchanges: BotExchange[] }
  /** A tool call, one row under the reply that made it. */
  | { id: string; kind: 'tool'; entry: Entry }
  /** The steps of a long run that are folded away. */
  | { id: string; kind: 'earlier'; count: number }

/** Your own message keeps its `user-<nonce>` id when the host confirms it, so the row doesn't redraw. */
export const stableId = (e: Entry) => (e.kind === 'user' && e.data.clientNonce ? `user-${e.data.clientNonce}` : e.id)

/**
 * The chat is flat: the chat's entries and every thread's replies in one list, oldest first by `createdAt`. An entry
 * that is in both the chat and a thread fetch counts once (the newer revision wins).
 */
export function flatEntries(entries: Entry[]): Entry[] {
  const byId = new Map<string, Entry>()
  for (const e of entries) {
    const known = byId.get(stableId(e))
    if (!known || e.rev >= known.rev) byId.set(stableId(e), e)
  }
  // Array.prototype.sort is stable: entries of one instant keep the order they arrived in.
  return [...byId.values()].sort((a, b) => a.createdAt - b.createdAt)
}

/**
 * Chat-visible entries plus time separators (gaps > 1 h), author grouping and tool rows. A bot's messages arrive
 * whole (Grok Bot's `send_message`); what it writes along the way is trace, except its tool calls: a run of them
 * shows its newest `VISIBLE_STEPS` and folds the rest. Runs of bot exchanges with one peer become one item.
 */
export function buildChat(entries: Entry[]): ChatItem[] {
  const out: ChatItem[] = []
  let lastDate: number | null = null
  let lastAuthor: string | null = null
  let run: Entry[] = []
  const flush = () => {
    // Tool steps between two replies break the run: the next agent reply opens a new group.
    if (run.length) lastAuthor = null
    if (run.length > VISIBLE_STEPS) out.push({ id: `earlier-${run[0]!.id}`, kind: 'earlier', count: run.length - VISIBLE_STEPS })
    for (const e of run.slice(-VISIBLE_STEPS)) out.push({ id: e.id, kind: 'tool', entry: e })
    run = []
  }
  for (const { entry: e, exchanges } of groupExchanges(flatEntries(entries), true)) {
    if (e.kind === 'tool') {
      run.push(e)
      // A long run counts as activity, so it doesn't make a gap of its own.
      if (lastDate !== null) lastDate = e.createdAt
      continue
    }
    const id = stableId(e)
    const at = e.createdAt
    // The tools sit above the separator of the message after a gap, not under it.
    flush()
    if (lastDate === null || at - lastDate > 3_600_000) {
      out.push({ id: `sep-${id}`, kind: 'separator', date: at })
      lastAuthor = null
    }
    if (exchanges) {
      out.push({ id, kind: 'exchanges', entry: e, exchanges })
      lastAuthor = null
      lastDate = at
      continue
    }
    // In a group each bot is its own author.
    const author = e.kind === 'user' ? 'user' : e.kind === 'agent' ? `agent:${e.data.author ?? ''}` : e.kind
    out.push({ id, kind: 'entry', entry: e, groupStart: author !== lastAuthor })
    lastAuthor = author === 'user' || e.kind === 'agent' ? author : null
    lastDate = at
  }
  flush()
  return out
}
