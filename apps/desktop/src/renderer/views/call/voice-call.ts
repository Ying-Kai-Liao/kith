import { isGroup, isWorking, needsInput } from '@shared/models'
import { isSecretCard } from '@shared/secret-card'
import { chatAndTasks } from '@shared/tasks'
import type { BotStore } from '../../store/bot-store'

/**
 * While a call's audio is active, speaks each new final reply in the bot's main chat (and its tasks) and
 * announces when the bot starts waiting for an approval (kit BotStore `beginVoiceCall`). Only
 * entries newer (by rev) than the call's start count. Returns the function that stops it.
 */
export function watchVoiceCall(store: BotStore, botId: string, handlers: {
    speak: (text: string) => void
    announce: (text: string) => void
    /** Growing text of a reply still being written (with `final` set once it's done); without it replies are only `speak`n when final. */
    stream?: (id: string, text: string, final: boolean) => void
    /** The bot started or stopped working. */
    working?: (working: boolean) => void
  },
) {
  let startRev = 0
  for (const b of store.bots.values()) startRev = Math.max(startRev, b.rev)
  for (const list of store.entries.values()) for (const e of list) startRev = Math.max(startRev, e.rev)
  // Replies already final when the call began are never spoken (a reaction bumps their rev).
  const finals = new Set(store.allEntries(botId).filter((e) => e.data.final).map((e) => e.id))
  let waiting = (() => {
    const bot = store.bots.get(botId)
    return bot ? needsInput(bot) : false
  })()

  // A group has no status of its own: its members' approvals arrive as cards.
  const cards = new Set(store.allEntries(botId).filter((e) => e.kind === 'permission').map((e) => e.id))
  // What `stream` was last given per entry: the host updates a streaming entry about every 300 ms.
  const streamed = new Map<string, string>()
  let working = (() => {
    const bot = store.bots.get(botId)
    return bot ? isWorking(bot) : false
  })()
  const nameOf = (author?: string) => (author ? store.bots.get(author)?.name : undefined)

  return store.subscribe(() => {
    const group = store.bots.get(botId)
    const inGroup = !!group && isGroup(group)
    const heard = new Set(chatAndTasks(store.allEntries(botId)).map((e) => e.id))
    for (const e of store.allEntries(botId)) {
      if (inGroup && e.kind === 'permission' && !cards.has(e.id)) {
        cards.add(e.id)
        // Key cards are for the eyes alone: nothing about them is said on a call.
        if (e.data.status === 'pending' && e.rev > startRev && !isSecretCard(e)) handlers.announce(`${nameOf(e.data.author) ?? 'A bot'} needs your approval in the chat.`)
      }
      // A shared file's entry carries only its name and size for the chat's card: never read aloud.
      if (e.data.files?.length) {
        if (e.data.final) finals.add(e.id)
        continue
      }
      if (handlers.stream && e.kind === 'agent' && !finals.has(e.id) && heard.has(e.id) && e.rev > startRev && e.data.text) {
        // In a group, a reply is said with its speaker's name.
        const speaker = inGroup ? nameOf(e.data.author) : undefined
        const text = speaker ? `${speaker}: ${e.data.text}` : e.data.text
        const final = !!e.data.final
        if (streamed.get(e.id) !== `${final}${text}`) {
          streamed.set(e.id, `${final}${text}`)
          handlers.stream(e.id, text, final)
        }
        if (final) {
          finals.add(e.id)
          streamed.delete(e.id)
        }
        continue
      }
      if (!e.data.final || finals.has(e.id)) continue
      finals.add(e.id)
      if (e.kind !== 'agent' || !heard.has(e.id) || e.rev <= startRev || !e.data.text) continue
      // In a group, a reply is said with its speaker's name.
      const speaker = inGroup ? nameOf(e.data.author) : undefined
      handlers.speak(speaker ? `${speaker}: ${e.data.text}` : e.data.text)
    }
    const bot = store.bots.get(botId)
    const now = bot ? needsInput(bot) : false
    if (bot && now && !waiting) handlers.announce(`${bot.name} needs your approval in the chat.`)
    waiting = now
    const busy = bot ? isWorking(bot) : false
    if (busy !== working) {
      working = busy
      handlers.working?.(busy)
    }
  })
}
