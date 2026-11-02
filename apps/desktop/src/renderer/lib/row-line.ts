import type { Bot } from '@shared/models'

/** What the row says when idle: the thread summary when the owner turned that on and the viewer may see one, else the last message. */
export function rowLine(bot: Pick<Bot, 'rowShowsSummary' | 'lastSummary' | 'lastMessage'>): string | null {
  const summary = bot.rowShowsSummary ? bot.lastSummary?.trim() : ''
  return summary || (bot.lastMessage ?? null)
}
