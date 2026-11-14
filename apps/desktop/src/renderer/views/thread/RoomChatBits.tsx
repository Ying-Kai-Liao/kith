import { useState } from 'react'
import type { Bot, Entry } from '@shared/models'
import { hiddenFromNames, hiddenMarkerHelp, hiddenMarkerText, isFirstJoin, joinLines, waitingAgent } from '@shared/room-people'
import { Hairline, Button } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { BotAvatar } from '../../components/Avatar'
import { HStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { useModel } from '../../lib/observable'
import { peopleFor } from '../../store/people-store'
import { roomPeopleFor } from '../../store/room-people-store'
import { useRoomFacts } from './room-facts'
import './room-people.css'

/** "Ben joined · sees from here" (the first join adds the rule-6 line), "Ben left": quiet dividers in the time-separator style. */
export function PersonLine({ entry }: { entry: Entry }) {
  const store = useStore()
  const lines = joinLines(entry, isFirstJoin(store.allEntries(entry.botId), entry))
  return (
    <>
      <HStack gap={10} className="chat-notice-divider">
        <Hairline grow />
        <Text size="caption2" tone="tertiary" className="chat-notice-divider-text">{lines[0]}</Text>
        <Hairline grow />
      </HStack>
      {lines[1] ? <Text as="div" size="caption2" tone="tertiary" className="room-line room-line-muted">{lines[1]}</Text> : null}
    </>
  )
}

/** "From before Ben joined" under a message Ben can't see, only below his join line (the divider explains the rest). */
export function HiddenMarker({ entry }: { entry: Entry }) {
  const store = useStore()
  const stays = useModel(roomPeopleFor(store, entry.botId)).stays
  const people = useModel(peopleFor(store)).people
  const [help, setHelp] = useState(false)
  const names = hiddenFromNames(entry, stays, (id) => people.find((p) => p.personId === id)?.name ?? 'Someone')
  if (!names.length) return null
  const agent = entry.kind === 'agent' ? store.authorName(entry.data.author ?? null) : null
  return (
    <div>
      <button className="room-hidden-marker press" aria-label={`${names.join(' and ')} can't see this message.`} onClick={() => setHelp((v) => !v)}>
        <Icon name="eye.slash" size={11} color="var(--secondary)" />
        {hiddenMarkerText(names)}
      </button>
      {help ? <Text as="div" size="caption" tone="secondary">{hiddenMarkerHelp(names, agent)}</Text> : null}
    </div>
  )
}

/** A room turn that didn't answer: one quiet line, and Send again for the viewer's own message (back in the box, not sent). */
export function UnansweredLine({ entry }: { entry: Entry }) {
  const store = useStore()
  const list = store.allEntries(entry.botId)
  const at = list.findIndex((e) => e.id === entry.id)
  // The message it answers is the newest of the viewer's own above it (the host names none).
  const own = list.slice(0, Math.max(at, 0)).reverse().find((e) => e.kind === 'user' && !e.data.sender && !e.data.hiddenFrom?.length && e.data.status !== 'cancelled')
  const agent = store.bots.get(entry.data.author ?? '')
  return (
    <HStack gap={8} className="room-unanswered chat-row">
      {agent ? <BotAvatar bot={agent} size={18} animated={false} /> : null}
      <Text size="footnote" tone="secondary" className="selectable">{entry.data.text}</Text>
      {own ? <Button kind="secondary" onClick={() => store.composerDrafts.set(entry.botId, null, own.data.text ?? '')}>Send again</Button> : null}
    </HStack>
  )
}

/** PR C's waiting notice, as a pointer into Kai's 1:1 with the agent (the card counts there, once). Kai only. */
export function WaitingPointer({ entry, agent }: { entry: Entry; agent: string }) {
  const store = useStore()
  const bot = [...store.bots.values()].find((b) => b.kind !== 'group' && (b.name === agent || b.hqAgent === agent))
  const card = bot ? store.allEntries(bot.id).find((e) => e.kind === 'permission' && e.data.room === entry.botId && e.data.status === 'pending') : undefined
  // Answered or closed: the row turns muted. Nothing open and no card found reads the same.
  const open = !!card
  return (
    <HStack gap={8} className="room-pointer chat-row">
      {bot ? <BotAvatar bot={bot} size={18} animated={false} /> : null}
      {open ? (
        <>
          <Text size="footnote" tone="text">{agent} is waiting for you</Text>
          <button className="press" onClick={() => bot && store.setSelection(bot.id)}>Open <Icon name="chevron.right" size={10} color="var(--secondary)" /></button>
        </>
      ) : (
        <Text size="footnote" tone="secondary">{agent} · you answered</Text>
      )}
    </HStack>
  )
}

/** The notice line a room's entry becomes: a join divider, the waiting pointer, else nothing special (null). */
export function roomNoticeFor(entry: Entry, chat: Bot | null, isOwner: boolean) {
  if (entry.data.notice === 'personJoined' || entry.data.notice === 'personLeft') return <PersonLine entry={entry} />
  const agent = isOwner && chat ? waitingAgent(entry) : null
  return agent ? <WaitingPointer entry={entry} agent={agent} /> : null
}

/** In the 1:1: where a room turn's card came from. Tap goes to the room. */
export function CardOrigin({ entry }: { entry: Entry }) {
  const store = useStore()
  const room = entry.data.room ? store.bots.get(entry.data.room) : undefined
  if (!room) return null
  return (
    <button className="room-origin press" onClick={() => store.setSelection(room.id)}>
      <Icon name="bubble.left.and.bubble.right" size={11} color="var(--secondary)" />
      From {room.name} · runs for everyone in {room.name}
    </button>
  )
}

/** Above Kai's box: which agents won't answer in this room, and why (a state, not a question; Fix opens Room info). */
export function WontAnswerLine({ group, fix }: { group: Bot; fix: () => void }) {
  const f = useRoomFacts(group)
  if (!f.enabled || !f.wont) return null
  return (
    <div className="room-wont" role="status">
      <Icon name="person.crop.circle.badge.xmark" size={16} color="var(--secondary)" />
      <Text size="footnote" tone="text">{f.wont.line}</Text>
      <button className="room-wont-fix press" onClick={fix}>Fix</button>
    </div>
  )
}
