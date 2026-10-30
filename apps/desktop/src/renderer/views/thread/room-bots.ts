import type { Bot } from '@shared/models'
import { botGuestsFor } from '../../store/bot-guests-store'
import type { BotStore } from '../../store/bot-store'

/** An outside bot (Grok) in a group: a member id the roster has no chat for. */
export interface RoomBot {
  id: string
  name: string
  provider: string
}

/** What the app knows of an outside bot: the owner's bot list, else the name its own messages carry. */
export function roomBotOf(store: BotStore, group: Bot | null | undefined, id: string): RoomBot {
  const guest = botGuestsFor(store).bots.find((b) => b.botId === id)
  const said = group ? store.allEntries(group.id).find((e) => e.data.sender?.botId === id)?.data.sender : undefined
  return { id, name: guest?.name ?? said?.name ?? 'Bot', provider: guest?.provider ?? said?.provider ?? '' }
}

/** The group's outside bots, in member order. */
export function roomBotsOf(store: BotStore, group: Bot): RoomBot[] {
  return group.members.filter((id) => !store.bots.has(id)).map((id) => roomBotOf(store, group, id))
}
