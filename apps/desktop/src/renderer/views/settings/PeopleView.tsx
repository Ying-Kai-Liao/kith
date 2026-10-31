import { useEffect, useMemo, useState } from 'react'
import QRCode from 'qrcode'
import { botsOf, ownersOwn } from '@shared/bot-guests'
import { isOpenInvite, type CreatedInvite, type PendingInvite, type Person, type Role } from '@shared/people'
import type { ProjectInfo } from '@shared/place-folders'
import { Button, CardForm, CardSection, IconButton, SegmentedChoice, Switch } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { Dialog, ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { HStack, Spacer, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import type { BotStore } from '../../store/bot-store'
import { useApp } from '../../store/context'
import { botGuestsFor } from '../../store/bot-guests-store'
import { peopleFor } from '../../store/people-store'
import { BotList, InviteBotSheet, OwnBotsSection, ownerAgents } from './BotGuestsView'
import { deviceIcon, relativeDay } from './parts'

/** Settings > People: who the owner invited, what they can use, and what they did. */
export function PeopleView() {
  const app = useApp()
  const local = app.local
  const people = useModel(local ? peopleFor(local) : null)
  const guests = useModel(local ? botGuestsFor(local) : null)
  const [inviting, setInviting] = useState<{ personId?: string } | null>(null)
  const [invitingBot, setInvitingBot] = useState(false)
  const [editing, setEditing] = useState<Person | null>(null)
  const [removing, setRemoving] = useState<Person | null>(null)
  const [activity, setActivity] = useState(false)
  const online = local?.connection.kind === 'online'
  useEffect(() => void (online && people ? people.load() : undefined), [online, people])
  useEffect(() => void (online && guests ? guests.load() : undefined), [online, guests])
  // A host before bot guests answers "unknown method": everything about them hides, without an error.
  const botsShown = guests?.supported === true
  if (!local || !people) return <CardForm><Text tone="secondary">People are kept by this computer's host, which isn't running.</Text></CardForm>

  const now = Date.now()
  const open = people.invites.filter((i) => isOpenInvite(i, now))
  const nameOf = (id: string) => people.people.find((p) => p.personId === id)?.name ?? id

  return (
    <CardForm>
      <CardSection
        title="People"
        footer={people.error ?? 'Invite someone to use some of your agents. Their chats and memory stay separate from yours.'}
      >
        {people.removed ? (
          <div className="settings-row">
            <Text grow>{people.removed.name} was removed.</Text>
            <Button kind="secondary" onClick={() => void people.restorePerson(people.removed!.personId)} disabled={people.busy}>Undo</Button>
            <IconButton title="Dismiss" icon="xmark" onClick={() => people.dismissRemoved()} />
          </div>
        ) : null}
        {people.people.length === 0 ? (
          <div className="settings-row"><Text tone="secondary">No one yet.</Text></div>
        ) : (
          people.people.map((person) => (
            <div key={person.personId}>
              <PersonRow person={person} busy={people.busy} onRestore={() => void people.restorePerson(person.personId)} onEdit={() => setEditing(person)} onInvite={() => setInviting({ personId: person.personId })} onRemove={() => setRemoving(person)} />
              {botsShown && guests ? <BotList store={local} bots={botsOf(guests.bots, person.personId)} view="owner" indent /> : null}
            </div>
          ))
        )}
        <div className="settings-row">
          <Button onClick={() => setInviting({})}>Invite someone</Button>
          {botsShown ? <Button kind="secondary" onClick={() => setInvitingBot(true)}>Invite a bot</Button> : null}
          <Spacer />
          <Button kind="secondary" onClick={() => { setActivity(true); void people.loadAudit() }}>Activity</Button>
        </div>
      </CardSection>

      {open.length > 0 ? (
        <CardSection title="Waiting to be used">
          {open.map((invite) => (
            <InviteRow key={invite.inviteId} invite={invite} name={nameOf(invite.personId)} busy={people.busy} onRevoke={() => void people.revokeInvite(invite.inviteId)} />
          ))}
        </CardSection>
      ) : null}

      {botsShown && guests ? <OwnBotsSection store={local} bots={ownersOwn(guests.bots)} onInvite={() => setInvitingBot(true)} /> : null}

      <Sheet className="settings-sheet" open={invitingBot} onClose={() => setInvitingBot(false)} width={480}>
        {invitingBot ? <InviteBotSheet store={local} onDone={() => setInvitingBot(false)} /> : null}
      </Sheet>
      <Sheet className="settings-sheet" open={inviting !== null} onClose={() => setInviting(null)} width={480}>
        {inviting ? <InviteSheet store={local} personId={inviting.personId} /> : null}
      </Sheet>
      <Sheet className="settings-sheet" open={editing !== null} onClose={() => setEditing(null)} width={480}>
        {editing ? <EditSheet store={local} person={editing} /> : null}
      </Sheet>
      <Sheet className="settings-sheet" open={activity} onClose={() => setActivity(false)} width={520}>
        <ActivitySheet store={local} />
      </Sheet>
      <Dialog
        open={removing !== null}
        title={`Remove ${removing?.name ?? 'this person'}?`}
        message="They lose access to your agents and their devices are signed out of this computer. Their memory and chats stay on this Mac."
        actions={[{ title: 'Remove', destructive: true, action: () => removing && void people.removePerson(removing.personId) }]}
        onClose={() => setRemoving(null)}
      />
    </CardForm>
  )
}

function PersonRow({ person, busy, onRestore, onEdit, onInvite, onRemove }: { person: Person; busy: boolean; onRestore: () => void; onEdit: () => void; onInvite: () => void; onRemove: () => void }) {
  const agents = Object.entries(person.agents)
  const seen = Math.max(0, ...person.devices.map((d) => d.lastSeenAt ?? 0))
  return (
    <VStack gap={6}>
      <div className="settings-row">
        <Icon name="person.crop.circle" size={26} color="var(--secondary)" />
        <VStack grow className="settings-stack">
          <Text truncate>{person.name}</Text>
          <Text size="caption" tone="secondary">
            {agents.length ? agents.map(([a, r]) => `${a} (${r})`).join(', ') : 'No agents'}
          </Text>
          <Text size="caption" tone="secondary">
            {person.revoked ? 'Access revoked' : !person.joined ? (person.openInvites ? 'Invite sent' : 'Hasn\'t joined') : seen ? `Last seen ${relativeDay(new Date(seen))}` : 'Joined'}
          </Text>
        </VStack>
        {person.revoked ? (
          <Button kind="secondary" onClick={onRestore} disabled={busy}>Restore access</Button>
        ) : (
          <>
            <IconButton title="New invite" icon="link" onClick={onInvite} />
            <IconButton title="Edit access" icon="slider.horizontal.3" onClick={onEdit} />
            <IconButton title="Remove" icon="minus.circle" onClick={onRemove} />
          </>
        )}
      </div>
      {person.devices.map((d) => (
        <HStack key={d.key} gap={8} className="settings-person-device">
          <Icon name={deviceIcon(d.platform)} size={12} color="var(--secondary)" />
          <Text size="caption" tone="secondary" truncate>
            {d.name ?? 'Device'} · {d.connected ? 'connected' : d.lastSeenAt ? `seen ${relativeDay(new Date(d.lastSeenAt))}` : 'not seen yet'}
          </Text>
        </HStack>
      ))}
    </VStack>
  )
}

function InviteRow({ invite, name, busy, onRevoke }: { invite: PendingInvite; name: string; busy: boolean; onRevoke: () => void }) {
  const hours = Math.max(1, Math.round((invite.expiresAt - Date.now()) / 3_600_000))
  return (
    <div className="settings-row">
      <Icon name="link" size={20} color="var(--secondary)" />
      <VStack grow className="settings-stack">
        <Text truncate>{name}</Text>
        <Text size="caption" tone="secondary">
          {Object.entries(invite.agents).map(([a, r]) => `${a} (${r})`).join(', ')} · expires in about {hours} h
        </Text>
      </VStack>
      <Button kind="secondary" onClick={onRevoke} disabled={busy}>Revoke</Button>
    </div>
  )
}

/** Tick agents and pick member or guest for each (guest by default). */
function AgentRoles({ agents, value, onChange }: { agents: { id: string; name: string }[]; value: Record<string, Role>; onChange: (v: Record<string, Role>) => void }) {
  if (agents.length === 0) return <Text size="callout" tone="secondary">No agents on this computer yet.</Text>
  return (
    <VStack gap={8}>
      {agents.map((agent) => {
        const role = value[agent.id]
        return (
          <HStack key={agent.id} gap={10} align="center">
            <Switch
              on={role !== undefined}
              label={`Give ${agent.name}`}
              onChange={(on) => {
                const next = { ...value }
                if (on) next[agent.id] = 'guest'
                else delete next[agent.id]
                onChange(next)
              }}
            >
              {agent.name}
            </Switch>
            <Spacer />
            {role ? (
              <SegmentedChoice<Role> selection={role} options={[{ id: 'guest', label: 'Guest' }, { id: 'member', label: 'Member' }]} onChange={(r) => onChange({ ...value, [agent.id]: r })} />
            ) : null}
          </HStack>
        )
      })}
      <Text size="footnote" tone="secondary">A guest asks you first. A member works within the limits you set for the agent.</Text>
    </VStack>
  )
}

function InviteSheet({ store, personId }: { store: BotStore; personId?: string }) {
  const people = peopleFor(store)
  useModel(people)
  const agents = useMemo(() => ownerAgents(store), [store])
  const existing = people.people
  const [pick, setPick] = useState<string>(personId ?? '')
  const [name, setName] = useState('')
  const [roles, setRoles] = useState<Record<string, Role>>({})
  const [made, setMade] = useState<CreatedInvite | null>(null)
  const chosen = existing.find((p) => p.personId === pick)
  useEffect(() => {
    if (chosen) setRoles(chosen.agents)
  }, [chosen])
  const ready = (pick !== '' || name.trim() !== '') && Object.keys(roles).length > 0

  const create = async () => {
    if (people.busy) return
    const result = await people.create(pick ? { personId: pick } : { name: name.trim() }, roles)
    if (result) setMade(result)
  }

  return (
    <SheetPage>
      <ModalHeader title={made ? 'Invite ready' : 'Invite someone'} />
      {made ? (
        <InviteLinkCard invite={made} />
      ) : (
        <VStack gap={14} className="settings-invite-body">
          {existing.length > 0 ? (
            <VStack gap={6}>
              <Text size="caption" tone="secondary">Who</Text>
              <select className="settings-select" value={pick} onChange={(e) => setPick(e.target.value)} aria-label="Person">
                <option value="">Someone new</option>
                {existing.map((p) => <option key={p.personId} value={p.personId}>{p.name}</option>)}
              </select>
            </VStack>
          ) : null}
          {pick === '' ? (
            <input className="settings-input" placeholder="Their name" value={name} onChange={(e) => setName(e.target.value)} aria-label="Their name" autoFocus />
          ) : null}
          <VStack gap={6}>
            <Text size="caption" tone="secondary">What they can use</Text>
            <AgentRoles agents={agents} value={roles} onChange={setRoles} />
          </VStack>
          {people.error ? <Text size="footnote" tone="danger">{people.error}</Text> : null}
          <HStack>
            <Spacer />
            <Button onClick={() => void create()} disabled={!ready || people.busy}>{people.busy ? 'Creating…' : 'Create invite'}</Button>
          </HStack>
        </VStack>
      )}
    </SheetPage>
  )
}

/** The link, Copy and a QR code. The link is a secret for one person: it stays on screen only. */
export function InviteLinkCard({ invite }: { invite: CreatedInvite }) {
  const [qr, setQr] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)
  useEffect(() => {
    let live = true
    void QRCode.toDataURL(invite.link, { errorCorrectionLevel: 'M', margin: 0, width: 400, color: { dark: '#000000', light: '#ffffff' } }).then((url) => live && setQr(url), () => {})
    return () => { live = false }
  }, [invite.link])
  return (
    <VStack gap={12} align="center" className="settings-invite-body">
      {qr ? <img src={qr} alt="Invite QR code" width={180} height={180} className="settings-invite-qr" /> : null}
      <HStack gap={8} className="settings-invite-link">
        <span className="settings-code selectable" aria-label="Invite link">{invite.link}</span>
        <Button kind="secondary" onClick={() => { window.kith.app.copy(invite.link); setCopied(true) }}>{copied ? 'Copied' : 'Copy'}</Button>
      </HStack>
      <Text size="callout" tone="secondary" className="settings-centered">
        Single use, expires in 24 hours. Send it to them; they sign in with Google, Apple or GitHub.
      </Text>
    </VStack>
  )
}

function EditSheet({ store, person: opened }: { store: BotStore; person: Person }) {
  const people = peopleFor(store)
  useModel(people)
  const guests = botGuestsFor(store)
  useModel(guests)
  // The toggle follows the refreshed list, not the person as they were when the sheet opened.
  const person = people.people.find((p) => p.personId === opened.personId) ?? opened
  const agents = useMemo(() => ownerAgents(store), [store])
  const [roles, setRoles] = useState<Record<string, Role>>(person.agents)
  const [projects, setProjects] = useState<string[]>(person.projects)
  const [available, setAvailable] = useState<ProjectInfo[]>([])
  const [saved, setSaved] = useState(false)
  useEffect(() => {
    void store.projects().then((r) => setAvailable(r.projects), () => {})
  }, [store])
  const save = async () => setSaved(await people.setAccess(person.personId, roles, projects))
  return (
    <SheetPage>
      <ModalHeader title={person.name} subtitle="What they can use" />
      <VStack gap={14} className="settings-invite-body">
        <AgentRoles agents={agents} value={roles} onChange={(r) => { setRoles(r); setSaved(false) }} />
        {available.length > 0 ? (
          <VStack gap={6}>
            <Text size="caption" tone="secondary">Shared projects</Text>
            {available.map((p) => (
              <Switch key={p.id} on={projects.includes(p.id)} onChange={(on) => { setProjects(on ? [...projects, p.id] : projects.filter((x) => x !== p.id)); setSaved(false) }}>
                {p.id}
              </Switch>
            ))}
          </VStack>
        ) : null}
        {guests.supported === true ? (
          <CardSection title={`${person.name}'s bots`} footer={`New bots ${person.name} invites start without waiting for you. Turning this on also allows any waiting now.`}>
            <div className="settings-row">
              <Switch on={person.autoBots} disabled={guests.working.has(person.personId)} onChange={(on) => void guests.setAutoAllow(person.personId, on)}>{`Always allow ${person.name}'s bots`}</Switch>
            </div>
            {botsOf(guests.bots, person.personId).length === 0 ? <div className="settings-row"><Text tone="secondary">No bots yet.</Text></div> : <BotList store={store} bots={botsOf(guests.bots, person.personId)} view="owner" />}
          </CardSection>
        ) : null}
        {people.error ? <Text size="footnote" tone="danger">{people.error}</Text> : null}
        {guests.error ? <Text size="footnote" tone="danger">{guests.error}</Text> : null}
        <HStack>
          <Text size="footnote" tone="secondary">{saved ? 'Saved' : ''}</Text>
          <Spacer />
          <Button onClick={() => void save()} disabled={people.busy}>Save</Button>
        </HStack>
      </VStack>
    </SheetPage>
  )
}

function ActivitySheet({ store }: { store: BotStore }) {
  const people = peopleFor(store)
  useModel(people)
  const nameOf = (id: string) => people.people.find((p) => p.personId === id)?.name ?? id
  return (
    <SheetPage>
      <ModalHeader title="Activity" subtitle="What invited people did on this computer" />
      <VStack gap={2} className="settings-invite-body settings-activity">
        {people.audit.length === 0 ? <Text tone="secondary">{people.busy ? 'Loading…' : 'Nothing yet.'}</Text> : null}
        {people.audit.map((row) => (
          <HStack key={row.id} gap={10}>
            <Text size="caption" tone="secondary" className="settings-activity-time">{new Date(row.at).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })}</Text>
            <Text size="caption" truncate grow>{nameOf(row.personId)} · {row.method}{row.target ? ` · ${row.target}` : ''}</Text>
            <Text size="caption" tone={row.allowed ? 'secondary' : 'danger'}>{row.allowed ? 'Allowed' : 'Denied'}</Text>
          </HStack>
        ))}
        {people.error ? <Text size="footnote" tone="danger">{people.error}</Text> : null}
      </VStack>
    </SheetPage>
  )
}
