import { useStore } from '../../store/context'
import { HqMemory } from './HqMemory'
import './memory.css'

/** A bot's memory: a kith agent has one, its plain files (`HqMemory`). A flagged native row has no agent to read, and a room has none of its own. */
export function MemoryCard({ botId }: { botId: string }) {
  const bot = useStore().bots.get(botId)
  // Until the bot is known, nothing shows: no flicker.
  if (!bot?.hqAgent) return null
  return <HqMemory botId={botId} />
}
