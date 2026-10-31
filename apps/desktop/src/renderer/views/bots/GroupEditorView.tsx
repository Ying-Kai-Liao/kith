import { useState } from 'react'
import { isGroup, type Bot } from '@shared/models'
import { BotAvatar, GroupAvatar } from '../../components/Avatar'
import { IconButton, Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { Dialog, ModalHeader, SheetPage, useDismiss } from '../../components/Overlay'
import { VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { parseLabels } from '@shared/room-labels'
import { errorText } from './drafts'
import { AutoTextArea } from './parts'
import './bots.css'

/**
 * Create a group chat, or change its name, what it's for and who's in it. Bots are
 * picked like recipients: chips on top, a search, and the bots not in it yet.
 */
export function GroupEditorView({ group, members: initialMembers = [] }: { group: Bot | null; members?: string[] }) {
  const store = useStore()
  const dismiss = useDismiss()
  // The group being edited; null creates one.
  const groupId = group?.id ?? null
  const [name, setName] = useState(group?.name ?? '')
  const [about, setAbout] = useState(group?.description ?? '')
  const [members, setMembers] = useState<string[]>(group?.members ?? initialMembers)
  const [labels, setLabels] = useState((group?.labels ?? []).join(', '))
  const [query, setQuery] = useState('')
  // A new group starts in the picker; an existing one shows its members first.
  const [adding, setAdding] = useState(group === null && initialMembers.length === 0)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [confirmDelete, setConfirmDelete] = useState<Bot | null>(null)

  const picked = members.map((id) => store.bots.get(id)).filter((b): b is Bot => !!b)
  // Bots that can still join, filtered by the search.
  const q = query.trim().toLowerCase()
  const candidates = store.roster.filter((b) => !isGroup(b) && !members.includes(b.id) && (!q || b.name.toLowerCase().includes(q)))
  const isNew = groupId === null
  const canSave = members.length > 0 && !saving
  // "Alice, Bob" when no name is typed.
  const defaultName = picked.length ? picked.map((b) => b.name).join(', ') : 'Name'
  const current = groupId ? store.bots.get(groupId) : undefined

  const toggle = (id: string) => {
    setMembers((list) => (list.includes(id) ? list.filter((m) => m !== id) : [...list, id]))
    setQuery('')
  }

  const save = () => {
    setSaving(true)
    setError(null)
    const typed = name.trim()
    const finalName = typed || defaultName
    const description = about.trim()
    const done = groupId
      ? store.updateGroup({ id: groupId, kind: 'group', name: finalName, description, members, labels: parseLabels(labels), pinned: store.bots.get(groupId)?.pinned })
      : store.createGroup(finalName, description, members, parseLabels(labels))
    void Promise.resolve(done)
      .then(() => dismiss())
      .catch((e: unknown) => setError(errorText(e)))
      .finally(() => setSaving(false))
  }

  return (
    <SheetPage>
      <ModalHeader title={isNew ? 'New group chat' : 'Group chat'} trailing={saving ? <Spinner /> : <IconButton title={isNew ? 'Create' : 'Save'} icon="checkmark" disabled={!canSave} onClick={save} />} />
      <div className="group-scroll">
        <div className="group-form">
          <div className="group-identity">
            <GroupAvatar members={picked} size={84} />
            <input className="group-name" value={name} placeholder={defaultName} aria-label="Name" onChange={(e) => setName(e.target.value)} />
            <AutoTextArea className="group-about" value={about} placeholder="What this group works on" minRows={1} maxRows={4} onChange={setAbout} />
          </div>
          <VStack>
            <div className="group-section-title">Labels</div>
            <input className="field-box" value={labels} placeholder="launch, q4 plan" aria-label="Labels" onChange={(e) => setLabels(e.target.value)} />
            <Text as="div" size="caption" tone="tertiary" className="group-note">Up to 5, separated by commas. They filter the room list.</Text>
          </VStack>
          <VStack>
            <div className="group-section-title">Members</div>
            {picked.map((bot) => (
              <div key={bot.id} className="member-row fade-in">
                <BotAvatar bot={bot} size={32} animated={false} />
                <span className="member-name">{bot.name}</span>
                <IconButton title={`Remove ${bot.name}`} icon="xmark" onClick={() => toggle(bot.id)} />
              </div>
            ))}
            {adding ? (
              <>
                <input
                  className="field-box fade-in group-search"
                  autoFocus
                  value={query}
                  placeholder="Search bots"
                  onChange={(e) => setQuery(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !e.nativeEvent.isComposing && candidates[0]) toggle(candidates[0].id)
                    if (e.key === 'Escape') setAdding(false)
                  }}
                />
                {candidates.map((bot) => (
                  <button key={bot.id} className="member-row press fade-in" aria-label={`Add ${bot.name}`} onClick={() => toggle(bot.id)}>
                    <BotAvatar bot={bot} size={32} animated={false} />
                    <Text tone="secondary" className="member-name">{bot.name}</Text>
                    <Icon name="plus.circle" size={18} color="var(--secondary)" />
                  </button>
                ))}
              </>
            ) : candidates.length ? (
              <button className="member-row press" onClick={() => setAdding(true)}>
                <span className="member-add-icon">
                  <Icon name="plus" size={16} color="var(--secondary)" />
                </span>
                <Text tone="secondary" className="member-name">Add Member</Text>
              </button>
            ) : null}
            <Text as="div" size="caption" tone="tertiary" className="group-note">Everyone answers in turn unless you @mention someone.</Text>
          </VStack>
          {error ? <Text size="footnote" tone="danger" className="fade-in">{error}</Text> : null}
          {current ? (
            <button className="press group-delete" onClick={() => setConfirmDelete(current)}>
              Delete group chat
            </button>
          ) : null}
        </div>
      </div>
      <DeleteBotDialog bot={confirmDelete} onClose={() => setConfirmDelete(null)} onDeleted={dismiss} />
    </SheetPage>
  )
}

/** The one "Delete <bot>?" confirmation (kit `deleteBotConfirmation`). */
function DeleteBotDialog({ bot, onClose, onDeleted }: { bot: Bot | null; onClose: () => void; onDeleted: () => void }) {
  const store = useStore()
  return (
    <Dialog
      open={bot !== null}
      title={`Delete ${bot?.name ?? 'bot'}?`}
      message={bot && isGroup(bot) ? 'Its bots and their own chats stay.' : 'Files it changed on your computer stay as they are.'}
      actions={[
        {
          title: 'Delete bot and its conversation',
          destructive: true,
          action: () => {
            if (bot) store.delete(bot)
            onDeleted()
          },
        },
      ]}
      onClose={onClose}
    />
  )
}
