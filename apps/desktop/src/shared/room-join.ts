// Which outside bots (Grok) the owner can add to a group room, and why the others can't. Only the owner's own bots,
// let in already, join a room; the host refuses any other id in a group's members.
import type { BotGuest } from './bot-guests.ts'

const providerWord = (provider: string) => (provider ? provider.charAt(0).toUpperCase() + provider.slice(1) : '')

/** Null when the bot can join; else the quiet line that says why not. */
export function cannotJoinWhy(bot: Pick<BotGuest, 'state' | 'ownerPerson' | 'ownerName'>): string | null {
  if (bot.ownerPerson) return `${bot.ownerName ? `${bot.ownerName}'s` : 'Another person’s'} bot · only your own bots join your rooms`
  switch (bot.state) {
    case 'active': return null
    case 'pending': return 'Waiting for you to let it in'
    case 'invited':
    case 'joining': return "Hasn't joined yet"
    case 'denied': return 'You turned it away'
    default: return 'Removed'
  }
}

/** The member row's second line: whose it is and where it runs. */
export function botRowLine(bot: Pick<BotGuest, 'provider' | 'ownerName' | 'ownerPerson'> | null): string {
  const who = bot?.ownerPerson ? `${bot.ownerName ?? 'Someone'}'s bot` : 'Your bot'
  const on = bot ? providerWord(bot.provider) : ''
  return on ? `${who} · runs on ${on}, outside this Mac` : `${who} · runs outside this Mac`
}

/** The bots list for the Add sheet: joinable first, then the ones that can't with their reasons. Bots already in the room are left out. */
export function addableBots<T extends Pick<BotGuest, 'botId' | 'name' | 'state' | 'ownerPerson' | 'ownerName'>>(bots: readonly T[], members: readonly string[]): { bot: T; why: string | null }[] {
  const rows = bots.filter((b) => !members.includes(b.botId)).map((bot) => ({ bot, why: cannotJoinWhy(bot) }))
  return [...rows.filter((r) => !r.why), ...rows.filter((r) => r.why)]
}
