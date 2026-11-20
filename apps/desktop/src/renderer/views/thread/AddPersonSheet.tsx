import { useEffect, useState } from 'react'
import type { Bot } from '@shared/models'
import { blockText, joinNames, missingFor, pickerGroups, type PickerRow, type RoomAgent } from '@shared/room-people'
import type { Person, Role } from '@shared/people'
import { Button, SegmentedChoice } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { ModalHeader, SheetPage, useDismiss } from '../../components/Overlay'
import { HStack, Spacer } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { InviteSheet } from '../settings/PeopleView'
import { useRoomFacts } from './room-facts'
import type { PersonPreset } from './RoomPeople'
import './conversation.css'
import './room-people.css'

/**
 * Room info › People › Add (SPEC §2): invited people split into "Can join" and "Needs access", the join card and the
 * grant card (give access and add in one step), and "Invite someone new" into this room. A room with an outside bot
 * takes no people, and the sheet says so instead of listing anyone.
 */
export function AddPersonSheet({ group, preset }: { group: Bot; preset: PersonPreset | null }) {
  const store = useStore()
  const dismiss = useDismiss()
  const f = useRoomFacts(group)
  const [pickedId, setPickedId] = useState<string | null>(preset?.personId ?? null)
  const [inviting, setInviting] = useState<boolean>(preset?.invite === true)
  const [roles, setRoles] = useState<Record<string, Role>>({})
  useEffect(() => setRoles({}), [pickedId])

  if (inviting) return <InviteSheet store={store} personId={preset?.invite ? preset.personId : undefined} room={{ id: group.id, name: group.name, agents: f.agents }} />

  const picked = f.people.people.find((p) => p.personId === pickedId) ?? null
  const { canJoin, needsAccess } = pickerGroups(f.people.people, f.model.stays, f.agents)
  const missing = picked ? missingFor(picked, f.agents) : []

  const row = (r: PickerRow) => (
    <button key={r.person.personId} className="press add-room-row" aria-pressed={pickedId === r.person.personId} onClick={() => setPickedId(r.person.personId)}>
      <Icon name="person.crop.circle" size={32} color="var(--secondary)" />
      <span className="add-room-name">
        <span>{r.person.name}</span>
        <Text as="span" size="footnote" tone="secondary">
          {r.missing.length ? `Doesn't have ${joinNames(r.missing.map((a) => a.name))}` : !r.accepted ? `Hasn't accepted the invite yet · sees ${group.name} once ${r.person.name} does` : `Has ${joinNames(f.agents.map((a) => a.name))}`}
        </Text>
      </span>
      <span className={`add-room-radio ${pickedId === r.person.personId ? 'on' : ''}`} aria-hidden>{pickedId === r.person.personId ? <Icon name="checkmark.circle.fill" size={20} color="var(--kith-ink)" /> : null}</span>
    </button>
  )

  const finish = async (person: Person, giveOnly: boolean) => {
    if (f.model.busy) return
    const given: Record<string, Role> = {}
    for (const a of missing) given[a.id] = roles[a.id] ?? 'guest'
    const names = missing.map((a) => a.name)
    const ok = missing.length ? (giveOnly ? await f.model.give(person, given) : await f.model.grantAndAdd(person, given, names)) : await f.model.add(person.personId)
    if (ok) dismiss()
  }

  return (
    <SheetPage>
      <ModalHeader title={`Add to ${group.name}`} />
      <div className="add-room-body">
        {f.block ? (
          <Text as="div" size="subheadline" tone="secondary" className="add-room-empty">{blockText(f.block, group.name)}</Text>
        ) : (
          <>
            {canJoin.length + needsAccess.length === 0 ? <Text as="div" size="subheadline" tone="secondary" className="add-room-empty">You haven't invited anyone yet.</Text> : null}
            {canJoin.length ? (
              <>
                <Text as="div" size="caption" weight="semibold" tone="secondary" className="add-room-heading">CAN JOIN</Text>
                <div className="add-room-list">{canJoin.map(row)}</div>
              </>
            ) : null}
            {needsAccess.length ? (
              <>
                <Text as="div" size="caption" weight="semibold" tone="secondary" className="add-room-heading">NEEDS ACCESS</Text>
                <div className="add-room-list">{needsAccess.map(row)}</div>
              </>
            ) : null}
            <div className="add-room-list room-invite-new">
              <button className="press add-room-row" onClick={() => setInviting(true)}>
                <Icon name="person.badge.plus" size={20} color="var(--secondary)" />
                <span className="add-room-name">Invite someone new…</span>
              </button>
            </div>
          </>
        )}
      </div>
      {picked && !f.block ? (
        <div className="add-room-card">
          {missing.length ? (
            <GrantCard person={picked} missing={missing} roles={roles} setRoles={setRoles} giveOnly={preset?.giveOnly === true && pickedId === preset.personId} />
          ) : (
            <JoinCard person={picked} agents={f.agents} room={group.name} />
          )}
          {f.model.error ? <Text as="div" size="footnote" tone="danger">{f.model.error}</Text> : null}
          <div className="add-room-actions">
            <Button onClick={() => void finish(picked, preset?.giveOnly === true && pickedId === preset.personId)} disabled={f.model.busy}>
              {missing.length ? (preset?.giveOnly && pickedId === preset.personId ? 'Give access' : 'Give access and add') : `Add ${picked.name}`}
            </Button>
            <Button kind="secondary" onClick={dismiss}>Cancel</Button>
          </div>
        </div>
      ) : null}
    </SheetPage>
  )
}

function JoinCard({ person, agents, room }: { person: Person; agents: RoomAgent[]; room: string }) {
  return (
    <>
      <div className="add-room-card-title"><Text size="body" weight="semibold" tone="text">{person.name} joins {room}.</Text></div>
      <div className="add-room-fact">
        <Icon name="eye" size={12} color="var(--secondary)" />
        <Text size="subheadline" tone="text">{person.name} sees what's said from now on, not before.</Text>
      </div>
      <div className="add-room-fact">
        <Icon name="at" size={12} color="var(--secondary)" />
        <Text size="subheadline" tone="text">{joinNames(agents.map((a) => a.name))} answer {person.name} only when {person.name} @'s them. Every card comes to you.</Text>
      </div>
    </>
  )
}

function GrantCard({ person, missing, roles, setRoles, giveOnly }: { person: Person; missing: RoomAgent[]; roles: Record<string, Role>; setRoles: (r: Record<string, Role>) => void; giveOnly: boolean }) {
  const names = joinNames(missing.map((a) => a.name))
  return (
    <>
      <div className="add-room-card-title"><Text size="body" weight="semibold" tone="text">{giveOnly ? `Give ${person.name} ${names}.` : `Give ${person.name} ${names} to add ${person.name}.`}</Text></div>
      {missing.map((a) => (
        <HStack key={a.id} gap={10} align="center">
          <Text size="body" tone="text">{a.name}</Text>
          <Spacer />
          <SegmentedChoice<Role> selection={roles[a.id] ?? 'guest'} options={[{ id: 'guest', label: 'Guest' }, { id: 'member', label: 'Member' }]} onChange={(r) => setRoles({ ...roles, [a.id]: r })} />
        </HStack>
      ))}
      <Text as="div" size="footnote" tone="secondary">{person.name} can then use {names} in a 1:1 too.</Text>
    </>
  )
}
