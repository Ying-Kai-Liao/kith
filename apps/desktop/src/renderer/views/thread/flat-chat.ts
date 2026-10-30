import { endLine, type Conversation } from '../../../shared/conversation.ts'
import { isChat, type Entry } from '../../../shared/models.ts'
import { isDeliveryTool } from '../../../shared/thread-feed.ts'
import { groupExchanges, type BotExchange } from './bot-exchange.ts'

export type ChatItem =
  | { id: string; kind: 'separator'; date: number }
  | { id: string; kind: 'entry'; entry: Entry; groupStart: boolean }
  | { id: string; kind: 'exchanges'; entry: Entry; exchanges: BotExchange[] }
  /** The tool calls between two messages as one collapsed "N steps" row; `id` is `steps-<first tool entry id>`. */
  | { id: string; kind: 'steps'; entries: Entry[] }
  /** Where a room run ended: "Used all 10 turns", "Worked it out · 6 of 10 turns", "You stopped it · 4 of 10 turns used". */
  | { id: string; kind: 'ending'; conversation: Conversation; text: string; more: boolean }

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
 * is one collapsed steps item (file deliveries left out; a run of only those shows nothing). Runs of bot exchanges with one peer become one item.
 */
export function buildChat(entries: Entry[], endings: Conversation[] = []): ChatItem[] {
  const out: ChatItem[] = []
  let lastDate: number | null = null
  let lastAuthor: string | null = null
  let run: Entry[] = []
  const flush = () => {
    // Tool steps between two replies break the run: the next agent reply opens a new group.
    if (run.length) lastAuthor = null
    const entries = run.filter((e) => !isDeliveryTool(e))
    if (entries.length) out.push({ id: `steps-${run[0]!.id}`, kind: 'steps', entries })
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
    const author = e.kind === 'user' ? (e.data.sender?.botId ? `bot:${e.data.sender.botId}` : 'user') : e.kind === 'agent' ? `agent:${e.data.author ?? ''}` : e.kind
    out.push({ id, kind: 'entry', entry: e, groupStart: author !== lastAuthor })
    lastAuthor = author === 'user' || e.kind === 'agent' || e.kind === 'user' ? author : null
    lastDate = at
  }
  flush()
  return endings.length ? withEndings(out, entries, endings) : out
}

/**
 * Each ended run's line goes after the last reply that belonged to it (a late one came after the end, so it doesn't
 * count), or after the message that opened it when nobody answered. A run whose messages aren't loaded has no place.
 */
function withEndings(items: ChatItem[], entries: Entry[], endings: Conversation[]): ChatItem[] {
  const flat = flatEntries(entries)
  const after = new Map<string, ChatItem[]>()
  for (const c of endings) {
    const line = endLine(c)
    if (!line) continue
    const replies = flat.filter((e) => e.data.conversation === c.id && e.data.late !== true && isChat(e))
    const anchor = replies.at(-1) ?? flat.find((e) => e.id === c.opening)
    if (!anchor) continue
    const key = stableId(anchor)
    after.set(key, [...(after.get(key) ?? []), { id: `end-${c.id}`, kind: 'ending', conversation: c, ...line }])
  }
  if (!after.size) return items
  return items.flatMap((item) => (item.kind === 'entry' && after.has(item.id) ? [item, ...after.get(item.id)!] : [item]))
}
