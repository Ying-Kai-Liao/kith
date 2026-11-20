import { useState } from 'react'
import { needsKithAgent, type Bot } from '@shared/models'
import { prefillOf } from '@shared/new-agent'
import { Button } from '../../components/Controls'
import { VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { errorText } from './drafts'
import { NewAgentSheet } from './NewAgentSheet'
import './bots.css'

/** What a flagged native row says in place of its composer and settings (one-bot-model C5). */
export const NATIVE_BOT_LINE = 'This bot no longer runs. Make a kith agent like it, or export what it kept.'

/** A host-native row the host refuses to run. Every other bot is a kith agent or a room. */
export const isFlagged = (bot: Bot): boolean => needsKithAgent(bot)

/**
 * A flagged row's two ways out: "Make a kith agent like this" (New agent, filled from the row) and "Export" (the host
 * writes the row's settings, memory and routines to a folder and says where). Nothing is converted or deleted here.
 */
export function NativeBotNotice({ bot }: { bot: Bot }) {
  const store = useStore()
  const [making, setMaking] = useState(false)
  const [exporting, setExporting] = useState(false)
  const [path, setPath] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const exportBundle = () => {
    if (exporting) return
    setExporting(true)
    setError(null)
    store.exportHostBot(bot.id).then(setPath).catch((e: unknown) => setError(errorText(e))).finally(() => setExporting(false))
  }

  return (
    <VStack gap={8} className="native-notice">
      <Text size="footnote" tone="attention">{NATIVE_BOT_LINE}</Text>
      {store.isOwner ? (
        <div className="native-notice-actions">
          {/* One flow at a time: a second click while the sheet is open does nothing. */}
          <Button disabled={making} onClick={() => setMaking(true)}>Make a kith agent like this</Button>
          <Button kind="secondary" disabled={exporting} onClick={exportBundle}>{exporting ? 'Exporting…' : 'Export'}</Button>
        </div>
      ) : null}
      {path ? <Text size="footnote" tone="secondary">{`Exported to ${path}`}</Text> : null}
      {error ? <Text size="footnote" tone="danger">{error}</Text> : null}
      <NewAgentSheet
        open={making}
        initialName={bot.name}
        prefill={prefillOf(bot)}
        onClose={() => setMaking(false)}
        onMade={(made) => { setMaking(false); store.setSelection(made.id) }}
      />
    </VStack>
  )
}
