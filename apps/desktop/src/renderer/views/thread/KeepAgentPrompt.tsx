import { useState, useSyncExternalStore } from 'react'
import type { Entry } from '@shared/models'
import { isTask } from '@shared/tasks'
import { keepNameError } from '@shared/quick-chat'
import { CardForm, CardSection, IconButton, Spinner } from '../../components/Controls'
import { ModalHeader, Sheet, SheetPage, type MenuItem } from '../../components/Overlay'
import { useStore as useLiveStore } from '../../store/context'
import type { BotStore } from '../../store/bot-store'
import { Field } from '../marketplace/pieces'

/** The name prompt is one sheet for the whole chat, like the topic prompt: a message's menu asks for it here. */
let asking: Entry | null = null
const listeners = new Set<() => void>()
const set = (next: Entry | null) => {
  asking = next
  for (const l of listeners) l()
}
const subscribe = (l: () => void) => {
  listeners.add(l)
  return () => void listeners.delete(l)
}

/** "Keep as agent" for the first message of a quick chat's thread: the owner's, and only in the built-in scratch bot's chat. */
export function keepAgentMenuItems(store: BotStore, entry: Entry): MenuItem[] {
  if (!store.isOwner || !isTask(entry) || !store.bots.get(entry.botId)?.builtin) return []
  return [{ title: 'Keep as agent…', detail: 'Make this chat a real agent', icon: 'person.crop.circle.badge.plus', action: () => set(entry) }]
}

export function KeepAgentPromptHost() {
  const current = useSyncExternalStore(subscribe, () => asking)
  return (
    <Sheet open={current !== null} onClose={() => set(null)} width={480}>
      {current ? <PromptPage key={current.id} root={current} /> : null}
    </Sheet>
  )
}

function PromptPage({ root }: { root: Entry }) {
  const store = useLiveStore()
  const [name, setName] = useState('')
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const problem = name.trim() ? keepNameError(name) : null
  const save = async () => {
    if (saving || !name.trim() || keepNameError(name)) return
    setSaving(true)
    setError(null)
    try {
      const { botId } = await store.keepAsAgent(root, name)
      set(null)
      if (botId) store.setSelection(botId)
    } catch (e) {
      // Name taken, a bad name: the host's own words, here, so the name can be changed.
      setError(e instanceof Error ? e.message : String(e))
      setSaving(false)
    }
  }
  return (
    <SheetPage>
      <ModalHeader
        title="Keep as agent"
        trailing={saving ? <Spinner /> : <IconButton title="Keep" icon="checkmark" disabled={!name.trim() || !!problem} onClick={() => { void save() }} />}
      />
      <CardForm>
        <CardSection footer={error ?? problem ?? 'Makes a real agent from this chat. It starts with a draft of its instructions and the same engine and model.'}>
          <Field value={name} onChange={setName} label="Name" placeholder="trip-planner" />
        </CardSection>
      </CardForm>
    </SheetPage>
  )
}
