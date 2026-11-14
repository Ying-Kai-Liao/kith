import { useState } from 'react'
import type { Bot } from '@shared/models'
import { joinNames, earlier, inTheRoom, invitedRows, lackLine, type InvitedRow, type NeedsKaiRow } from '@shared/room-people'
import type { Person } from '@shared/people'
import { Dialog } from '../../components/Overlay'
import { Icon } from '../../components/Icon'
import { Text } from '../../components/Text'
import { VStack } from '../../components/Stack'
import { useStore } from '../../store/context'
import { openSettingsWindow } from '../settings/profile-menu'
import { useRoomFacts } from './room-facts'
import './room-people.css'

/** What the Add sheet opens on: a person to give agents to (needs-you row, Invite again) or add. */
export interface PersonPreset {
  personId?: string
  /** Give the missing agents only; the person is in the room already. */
  giveOnly?: boolean
  /** Open "Invite someone new" (Invite again opens it with the person picked). */
  invite?: boolean
}

const hoursAgo = (ms: number) => {
  const h = Math.max(1, Math.round((Date.now() - ms) / 3_600_000))
  return h >= 48 ? `Seen ${Math.round(h / 24)} d ago` : `Seen ${h} h ago`
}

/** "Active now", "Seen 2 h ago"; nothing when no device was ever seen. */
export function seenLine(person: Person): string | null {
  if (person.devices.some((d) => d.connected)) return 'Active now'
  const last = Math.max(0, ...person.devices.map((d) => d.lastSeenAt ?? 0))
  return last ? hoursAgo(last) : null
}

/**
 * Room info › People (SPEC §1). Needs-you rows first, then Kai, people in the room, invited, and a collapsed "Earlier".
 * Hidden while the host's `rooms.people` setting is off: then only the owner's own row shows, as before.
 */
export function RoomPeopleSection({ group, openAdd }: { group: Bot; openAdd: (preset?: PersonPreset) => void }) {
  const store = useStore()
  const f = useRoomFacts(group)
  const [removing, setRemoving] = useState<Person | null>(null)
  const [cancelling, setCancelling] = useState<InvitedRow | null>(null)
  const [showEarlier, setShowEarlier] = useState(false)
  const stays = f.model.stays
  const members = inTheRoom(stays, f.people.people, f.agents)
  const invited = invitedRows(f.people.invites, f.people.people, group.id, f.agents, Date.now())
  const gone = earlier(stays, f.people.people).filter((p) => !invited.some((i) => i.invite.personId === p.personId))
  const busy = f.model.busy

  const head = (
    <div className="details-members-head">
      <Text size="caption" weight="semibold" tone="secondary">PEOPLE</Text>
      {f.enabled && store.isOwner ? <button className="press details-members-add" onClick={() => openAdd()}>Add</button> : null}
    </div>
  )
  const you = (
    <div className="member-row">
      <span className="room-person-pic"><Icon name="person.crop.circle" size={32} color="var(--secondary)" /></span>
      <VStack as="span" gap={1} grow>
        <span className="member-name">You</span>
        <Text size="compactSecondary" tone="secondary">Owner</Text>
      </VStack>
    </div>
  )
  if (!f.enabled) return <div className="room-people" id="room-people">{head}{you}</div>

  const needsRow = (r: NeedsKaiRow) => {
    const person = f.people.people.find((p) => p.personId === r.personId)!
    return (
      <div key={r.personId} className="room-needs">
        <Text size="subheadline" weight="semibold" tone="text">{r.name} can't open {group.name}</Text>
        <Text size="compactSecondary" tone="secondary">
          {r.kind === 'revoked' ? `${r.name}'s access is revoked. No agent answers here until ${r.name} is removed or restored.` : lackLine(r.missing)}
        </Text>
        <div className="room-row-actions">
          {r.kind === 'missing' ? (
            <>
              <button className="room-action ink press" disabled={busy} onClick={() => openAdd({ personId: r.personId, giveOnly: true })}>Give {joinNames(r.missing.map((a) => a.name))}</button>
              <button className="room-action press" disabled={busy} onClick={() => setRemoving(person)}>Remove</button>
            </>
          ) : (
            <>
              <button className="room-action ink press" disabled={busy} onClick={() => setRemoving(person)}>Remove</button>
              <button className="room-action press" onClick={() => openSettingsWindow('people')}>Restore…</button>
            </>
          )}
        </div>
      </div>
    )
  }

  const invitedLine = (row: InvitedRow) =>
    row.state === 'ranOut' ? `${row.name}'s invite ran out`
    : row.state === 'waiting' ? `Invited · joins when ${row.name} accepts`
    : row.missing.length ? `Accepted · joins once ${row.name} has ${joinNames(row.missing.map((a) => a.name))}`
    : 'Accepted · joining…'

  return (
    <div className="room-people" id="room-people">
      {head}
      {f.needs.map(needsRow)}
      {you}
      {members.map((person) => (
        <div key={person.personId} className="member-row removable">
          <span className="room-person-pic"><Icon name="person.crop.circle" size={32} color="var(--secondary)" /></span>
          <VStack as="span" gap={1} grow>
            <span className="member-name">{person.name}</span>
            {seenLine(person) ? <Text size="compactSecondary" tone="secondary">{seenLine(person)}</Text> : null}
          </VStack>
          {store.isOwner ? <button className="press bot-member-remove" disabled={busy} onClick={() => setRemoving(person)}>Remove</button> : null}
        </div>
      ))}
      {invited.map((row) => (
        <div key={row.invite.inviteId} className="member-row removable">
          <span className="room-person-pic"><Icon name="person.crop.circle.badge.plus" size={32} color="var(--secondary)" /></span>
          <VStack as="span" gap={1} grow>
            <span className="member-name">{row.name}</span>
            <Text size="compactSecondary" tone="secondary">{invitedLine(row)}</Text>
          </VStack>
          {row.state === 'ranOut' ? (
            <button className="press bot-member-remove" onClick={() => openAdd({ personId: row.invite.personId, invite: true })}>Invite again</button>
          ) : row.state === 'accepted' && row.missing.length ? (
            <button className="press bot-member-remove" disabled={busy} onClick={() => openAdd({ personId: row.invite.personId, giveOnly: true })}>Give {joinNames(row.missing.map((a) => a.name))}</button>
          ) : row.state === 'waiting' ? (
            <button className="press bot-member-remove" onClick={() => setCancelling(row)}>Cancel</button>
          ) : null}
        </div>
      ))}
      {gone.length ? (
        <>
          <button className="member-row press" aria-expanded={showEarlier} onClick={() => setShowEarlier((v) => !v)}>
            <span className="room-person-pic"><Icon name={showEarlier ? 'chevron.down' : 'chevron.right'} size={12} color="var(--secondary)" /></span>
            <Text tone="secondary" className="member-name">Earlier · {gone.length} {gone.length === 1 ? 'person' : 'people'}</Text>
          </button>
          {showEarlier
            ? gone.map((person) => (
                <div key={person.personId} className="member-row removable">
                  <span className="room-person-pic" />
                  <span className="member-name">{person.name}</span>
                  <button className="press bot-member-remove" disabled={busy} onClick={() => openAdd({ personId: person.personId })}>Add back</button>
                </div>
              ))
            : null}
        </>
      ) : null}
      {members.length + f.needs.length + invited.length > 0 ? (
        <Text as="div" size="footnote" tone="secondary" className="room-people-footer">
          People see what's said from when they join. Agents answer them only when @'d. Every card comes to you.
        </Text>
      ) : null}
      {f.model.error ? <Text as="div" size="footnote" tone="danger" className="room-people-footer">{f.model.error}</Text> : null}
      <Dialog
        open={removing !== null}
        title={`Remove ${removing?.name ?? 'this person'} from ${group.name}?`}
        message={`${removing?.name ?? 'They'} stops seeing ${group.name} now. If you add ${removing?.name ?? 'them'} again, ${removing?.name ?? 'they'} sees only what's said after that.`}
        actions={[{ title: 'Remove', destructive: true, action: () => removing && void f.model.remove(removing.personId) }]}
        onClose={() => setRemoving(null)}
      />
      <Dialog
        open={cancelling !== null}
        title={`Cancel ${cancelling?.name ?? 'the'}'s invite?`}
        message={`The link stops working. ${cancelling?.name ?? 'They'} won't join ${group.name}.`}
        actions={[{ title: 'Cancel invite', destructive: true, action: () => cancelling && void f.people.revokeInvite(cancelling.invite.inviteId) }]}
        onClose={() => setCancelling(null)}
      />
    </div>
  )
}
