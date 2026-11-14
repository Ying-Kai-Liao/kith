import { useEffect, useState } from 'react'
import type { Bot } from '@shared/models'
import { BEN_START_LINES, askedLine, ownerLine } from '@shared/room-people'
import { Button } from '../../components/Controls'
import { Dialog } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import './room-people.css'

const POLL_MS = 8000

/**
 * An invited person's room (SPEC §8-§10): a group in their roster whose messages they read from when they were added.
 * The host pushes nothing to people here, so the open room is re-read every few seconds and on focus.
 */
export function useMemberRoom(botId: string) {
  const store = useStore()
  const inRoom = store.myRooms.has(botId)
  useEffect(() => {
    if (!inRoom) return
    // The list first: a new stay drops the old messages before the room is read again.
    const refresh = () => void store.refreshMyRooms().then(() => store.loadRoom(botId))
    refresh()
    const timer = setInterval(refresh, POLL_MS)
    const onFocus = refresh
    window.addEventListener('focus', onFocus)
    return () => {
      clearInterval(timer)
      window.removeEventListener('focus', onFocus)
    }
  }, [store, botId, inRoom])
  return inRoom
}

/** Room info for a person: the room's agents, whose room it is, and Leave. No people list (their app has no source for it). */
export function MemberRoomInfo({ room }: { room: Bot }) {
  const store = useStore()
  const info = store.myRooms.get(room.id)
  const [leaving, setLeaving] = useState(false)
  return (
    <div className="room-people" style={{ padding: '0 20px' }}>
      <Text as="div" size="caption" weight="semibold" tone="secondary" className="details-members-head">AGENTS</Text>
      {(info?.agents ?? []).map((name) => (
        <div key={name} className="member-row">
          <VStackName name={name} />
        </div>
      ))}
      <Text as="div" size="footnote" tone="secondary" className="room-people-footer">
        {ownerLine(room.name)}
      </Text>
      <button className="press bot-member-remove room-leave" onClick={() => setLeaving(true)}>Leave {room.name}</button>
      <Dialog
        open={leaving}
        title={`Leave ${room.name}?`}
        message={`You stop seeing ${room.name}. Kai can add you again.`}
        actions={[{ title: 'Leave', destructive: true, action: () => void store.leaveRoom(room.id) }]}
        onClose={() => setLeaving(false)}
      />
    </div>
  )
}

function VStackName({ name }: { name: string }) {
  return (
    <span className="member-name">
      {name}
      <Text as="span" size="compactSecondary" tone="secondary"> · Answers when @'d</Text>
    </span>
  )
}

/** The one neutral state for a room that is gone: left on another device, removed, or closed (the app can't tell, and doesn't guess). */
export function RoomClosed({ name, back }: { name: string; back: () => void }) {
  return (
    <div className="room-closed">
      <Text size="body" tone="text">You're no longer in {name}.</Text>
      <Button kind="secondary" onClick={back}>Back</Button>
    </div>
  )
}

/** The two opening lines at the top of a person's history, once no older page is left. */
export function MemberStartLines({ room }: { room: string }) {
  return (
    <div className="room-line room-line-muted" style={{ flexDirection: 'column', gap: 2, padding: '12px 0' }}>
      {BEN_START_LINES(room).map((l) => <span key={l}>{l}</span>)}
    </div>
  )
}

/** "Asked planner" under the person's newest message that @'d an agent, until an agent reply lands. */
export function AskedLine({ botId }: { botId: string }) {
  const store = useStore()
  const info = store.myRooms.get(botId)
  const line = info ? askedLine(store.chat(botId), info.agents) : null
  return line ? <Text as="div" size="footnote" tone="secondary" className="room-line room-line-muted" style={{ justifyContent: 'flex-end' }}>{line}</Text> : null
}
