import { nestBotGuestChats } from './bot-guest-chat.ts'
import { isWorking, needsInput, type Bot } from './models.ts'

export type CrewOrder = 'automatic' | 'manual'

/** Automatic's tiers: pinned, then the ones waiting on you, then the ones working, then the rest. */
function tier(bot: Bot): number {
  if (bot.pinned) return 0
  if (needsInput(bot)) return 1
  if (isWorking(bot)) return 2
  return 3
}

/**
 * A computer's rows in the order the Crew sidebar shows them. Manual keeps the given (drag) order; Automatic sorts by
 * tier, then newest message first within working and recent. Pinned and needs-you rows keep the drag order among
 * themselves, and the sort is stable, so ties do too.
 */
export function crewRows<T extends { bot: Bot }>(items: readonly T[], order: CrewOrder | null | undefined): T[] {
  if (order === 'manual') return nestBotGuestChats(items, (i) => i.bot)
  const sorted = items
    .map((item, index) => ({ item, index }))
    .sort((a, b) => {
      const ta = tier(a.item.bot)
      const tb = tier(b.item.bot)
      if (ta !== tb) return ta - tb
      if (ta >= 2 && a.item.bot.lastAt !== b.item.bot.lastAt) return b.item.bot.lastAt - a.item.bot.lastAt
      return a.index - b.index
    })
    .map(({ item }) => item)
  return nestBotGuestChats(sorted, (i) => i.bot)
}
