import { useState, useSyncExternalStore } from 'react'
import type { Entry } from '@shared/models'
import { foldTopics, TOPIC_TITLE_MAX } from '@shared/topics'
import { CardForm, CardSection, IconButton, Spinner } from '../../components/Controls'
import { ModalHeader, Sheet, SheetPage, type MenuItem } from '../../components/Overlay'
import { useStore as useLiveStore } from '../../store/context'
import type { BotStore } from '../../store/bot-store'
import { Field } from '../marketplace/pieces'

/** The title prompt is one sheet for the whole chat: any bubble or card asks for it here. */
type Asking = { entry: Entry; rename: boolean }
let asking: Asking | null = null
const listeners = new Set<() => void>()
const set = (next: Asking | null) => {
  asking = next
  for (const l of listeners) l()
}
const subscribe = (l: () => void) => {
  listeners.add(l)
  return () => void listeners.delete(l)
}

export const askMakeTopic = (entry: Entry) => set({ entry, rename: false })
export const askRenameTopic = (entry: Entry) => set({ entry, rename: true })

/** What the prompt starts with: the message's first line, cut to what a title can be. */
export const suggestedTitle = (text: string | undefined) => (text ?? '').trim().split('\n')[0]!.trim().slice(0, TOPIC_TITLE_MAX)

/**
 * "Make a topic from here" for a message's menu: the owner's, and not on a message that is already part of a topic
 * (inside one, or the one that starts it).
 */
export function topicMenuItems(store: BotStore, entry: Entry): MenuItem[] {
  if (!store.isOwner || entry.data.topic) return []
  const { hidden } = foldTopics(store.allEntries(entry.botId))
  if ([...hidden].some((e) => e.id === entry.id)) return []
  return [{ title: 'Make a topic from here', detail: 'This and the replies after it', icon: 'text.bubble', action: () => askMakeTopic(entry) }]
}

export function TopicPromptHost() {
  const current = useSyncExternalStore(subscribe, () => asking)
  return (
    <Sheet open={current !== null} onClose={() => set(null)} width={480}>
      {current ? <PromptPage key={current.entry.id + current.rename} {...current} /> : null}
    </Sheet>
  )
}

function PromptPage({ entry, rename }: Asking) {
  const store = useLiveStore()
  const [title, setTitle] = useState(rename ? (entry.data.topic?.title ?? '') : suggestedTitle(entry.data.text))
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const save = async () => {
    if (saving || !title.trim()) return
    setSaving(true)
    setError(null)
    try {
      await (rename ? store.renameTopic(entry, title.trim()) : store.makeTopic(entry, title.trim()))
      set(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      setSaving(false)
    }
  }
  return (
    <SheetPage>
      <ModalHeader
        title={rename ? 'Rename topic' : 'Make a topic'}
        trailing={saving ? <Spinner /> : <IconButton title="Save" icon="checkmark" disabled={!title.trim()} onClick={() => { void save() }} />}
      />
      <CardForm>
        <CardSection footer={error ?? 'The room folds into one card with this title. Unfold it any time.'}>
          <Field value={title} onChange={(v) => setTitle(v.slice(0, TOPIC_TITLE_MAX))} label="Title" placeholder="What is this about?" />
        </CardSection>
      </CardForm>
    </SheetPage>
  )
}
