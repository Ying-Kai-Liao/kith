// A bot guest's chat: the per-person copy of an agent the host keeps for one outside bot's conversations. The
// apps show it as the agent, tagged with who asks, and read-only (v1: nothing is written into it from here).
import type { Bot } from './models.ts'

export const isBotGuestChat = (b: Pick<Bot, 'botGuest'> | null | undefined): boolean => !!b?.botGuest

const providerWord = (provider: string) => (provider ? provider.charAt(0).toUpperCase() + provider.slice(1) : 'a bot')

/** "from Grok · Sam's Rex"; "from Grok · Your Rex" for the computer owner's own bot. */
export function botGuestTag(g: NonNullable<Bot['botGuest']>): string {
  const provider = g.provider ? providerWord(g.provider) : 'a bot'
  return `from ${provider} · ${g.ownerName ? `${g.ownerName}'s` : 'Your'} ${g.name}`
}

/** The quiet line that stands in for the composer. */
export const botGuestReadOnlyLine = (g: NonNullable<Bot['botGuest']>, agentName: string): string =>
  `${g.name} asks ${agentName} from ${g.provider ? providerWord(g.provider) : 'outside'}. You can read along here.`

/** The rows with each bot-guest chat moved right after its agent's own chat (one whose agent has no chat here goes last). */
export function nestBotGuestChats<T>(items: readonly T[], bot: (t: T) => Pick<Bot, 'id' | 'hqAgent' | 'botGuest'> = (t) => t as never): T[] {
  const guests = items.filter((t) => isBotGuestChat(bot(t)))
  if (!guests.length) return [...items]
  const placed = new Set<T>()
  const out: T[] = []
  for (const t of items) {
    if (isBotGuestChat(bot(t))) continue
    out.push(t)
    const agent = bot(t).hqAgent
    if (!agent) continue
    for (const g of guests) if (bot(g).hqAgent === agent && !placed.has(g)) (out.push(g), placed.add(g))
  }
  for (const g of guests) if (!placed.has(g)) out.push(g)
  return out
}

/** The tag on a thread a bot started: the host's own label ("from Grok · Kai's Grok"), else built from `originBot`. */
export function botOriginLabel(t: { originLabel?: string; originBot?: { provider?: string; owner?: string; name?: string } }): string | undefined {
  const label = t.originLabel?.trim()
  if (label) return label
  const b = t.originBot
  return b ? `from ${b.provider ? providerWord(b.provider) : 'a bot'}${b.name ? ` · ${b.name}` : ''}` : undefined
}

/**
 * In a bot-guest chat, a user-kind entry the outside bot wrote (it carries `data.sender.botId`) is the other party's
 * message, not the owner's. Null everywhere else: normal chats, other kinds, entries without a sender.
 */
export function botGuestSender(chat: Pick<Bot, 'botGuest'> | null | undefined, entry: { kind: string; data: { sender?: { botId?: string; name?: string; speaker?: string } } }): { name: string } | null {
  const s = entry.data.sender
  if (!isBotGuestChat(chat) || entry.kind !== 'user' || !s?.botId) return null
  const name = s.name?.trim() || chat!.botGuest!.name
  const speaker = s.speaker?.trim()
  return { name: speaker ? `${name} · ${speaker}` : name }
}
