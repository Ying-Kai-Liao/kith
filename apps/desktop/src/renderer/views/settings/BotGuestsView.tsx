import { useEffect, useMemo, useState } from 'react'
import { avatarImage, DEFAULT_IMAGE } from '@shared/crew'
import {
  allowedBy, ago, botLine, botTitle, detailSubtitle, inTime, isLiveBot, limitsLine, providerLabel,
  type BotGuest, type BotProvider, type CreatedBotInvite,
} from '@shared/bot-guests'
import { isUnsupportedHost } from '@shared/kith-info'
import { money, ofLimit, type BotsCap } from '@shared/usage-limits'
import { Button, CardForm, CardSection, Disclosure, SegmentedChoice, Spinner, Switch, ValueRow } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { BotAvatar, BotGuestAvatar } from '../../components/kith'
import { Dialog, ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { HStack, Spacer, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useModel, useModels } from '../../lib/observable'
import { botGuestsFor } from '../../store/bot-guests-store'
import type { BotStore } from '../../store/bot-store'
import { useApp } from '../../store/context'
import { BudgetPicker } from '../usage/PeopleBudgets'
import './settings.css'

/** An agent a bot can be given. */
export interface OfferedAgent {
  id: string
  name: string
}

/** The owner's agents on this computer: one per hq agent. */
export function ownerAgents(store: BotStore): OfferedAgent[] {
  const seen = new Map<string, string>()
  for (const bot of store.roster) if (bot.hqAgent && !seen.has(bot.hqAgent)) seen.set(bot.hqAgent, bot.name)
  return [...seen].map(([id, name]) => ({ id, name }))
}

/** What a bot can be given here: the owner's agents, or for a member only those they hold (a bot has at most what its owner has). */
export function offeredAgents(store: BotStore): OfferedAgent[] {
  const granted = store.granted
  return granted ? granted.map((g) => ({ id: g.agent, name: g.name ?? g.agent })) : ownerAgents(store)
}

const stateWord: Record<BotGuest['state'], string> = {
  invited: 'Invited',
  joining: 'Joining',
  pending: 'Waiting for an answer',
  active: 'Allowed',
  denied: 'Said no',
  revoked: 'Removed',
}

/** The bots in a list: live ones, then the ended folded away; each row opens what it can (a detail, a new link). */
export function BotList({ store, bots, view, indent = false }: { store: BotStore; bots: BotGuest[]; view: 'owner' | 'invitee'; indent?: boolean }) {
  const model = botGuestsFor(store)
  useModel(model)
  const [detail, setDetail] = useState<string | null>(null)
  const [fresh, setFresh] = useState<{ bot: BotGuest; invite: CreatedBotInvite } | null>(null)
  const [ended, setEnded] = useState(false)
  const now = Date.now()
  const live = bots.filter(isLiveBot)
  const over = bots.filter((b) => !isLiveBot(b))
  const open = bots.find((b) => b.botId === detail) ?? null
  const row = (bot: BotGuest) => (
    <BotRow
      key={bot.botId}
      bot={bot}
      view={view}
      now={now}
      owner={ownerOf(store)}
      indent={indent}
      busy={model.working.has(bot.botId)}
      onOpen={() => setDetail(bot.botId)}
      onDecide={(d) => void model.decide(bot, d)}
      onCancel={() => void model.revoke(bot.botId)}
      onNewLink={() => void model.renew(bot).then((invite) => invite && setFresh({ bot, invite }))}
    />
  )
  return (
    <VStack gap={6} className="settings-bot-list">
      {live.map(row)}
      {over.length > 0 ? (
        <div className={indent ? 'settings-bot-indent' : undefined}>
          <Disclosure expanded={ended} onToggle={() => setEnded(!ended)} label={`Ended (${over.length})`}>
            {over.map(row)}
          </Disclosure>
        </div>
      ) : null}
      <Sheet className="settings-sheet" open={open !== null} onClose={() => setDetail(null)} width={480}>
        {open ? <BotDetail store={store} bot={open} view={view} onClose={() => setDetail(null)} /> : null}
      </Sheet>
      <Sheet className="settings-sheet" open={fresh !== null} onClose={() => setFresh(null)} width={480}>
        {fresh ? (
          <SheetPage>
            <ModalHeader title="Invite ready" subtitle={botTitle(fresh.bot)} />
            <BotInviteResult bot={fresh.bot} invite={fresh.invite} view={view} ownerName={ownerOf(store)} onDone={() => setFresh(null)} />
          </SheetPage>
        ) : null}
      </Sheet>
    </VStack>
  )
}

function BotRow({ bot, view, now, owner, indent, busy, onOpen, onDecide, onCancel, onNewLink }: {
  bot: BotGuest
  view: 'owner' | 'invitee'
  now: number
  owner: string
  indent: boolean
  busy: boolean
  onOpen: () => void
  onDecide: (d: 'allow' | 'deny') => void
  onCancel: () => void
  onNewLink: () => void
}) {
  const { line, trailing, emphasis } = botLine(bot, view, now, owner)
  const provider = providerLabel(bot.provider)
  const body = (
    <>
      <BotGuestAvatar provider={bot.provider} size={26} />
      <VStack grow className="settings-stack">
        <Text truncate>
          {bot.name}
          {provider ? <Text tone="secondary">{` · ${provider}`}</Text> : null}
        </Text>
        <Text size="caption" tone={emphasis ? 'text' : 'secondary'} className={emphasis ? 'settings-bot-waiting' : undefined}>{line}</Text>
      </VStack>
    </>
  )
  const cls = `settings-row settings-bot-row${indent ? ' settings-bot-indent' : ''}`
  if (trailing === 'chevron') {
    return (
      <button className={`${cls} settings-nav-row`} onClick={onOpen} aria-label={`${bot.name}, details`}>
        {body}
        <Icon name="chevron.right" size={11} color="var(--tertiary)" />
      </button>
    )
  }
  return (
    <div className={cls}>
      {body}
      {trailing === 'spinner' ? <Spinner size={14} /> : null}
      {trailing === 'cancel' ? (
        <>
          <Button kind="secondary" onClick={onNewLink} disabled={busy}>New link</Button>
          <Button kind="secondary" onClick={onCancel} disabled={busy}>Cancel invite</Button>
        </>
      ) : null}
      {trailing === 'decide' ? (
        <>
          <Button kind="secondary" onClick={() => onDecide('deny')} disabled={busy}>Deny</Button>
          <Button onClick={() => onDecide('allow')} disabled={busy}>Allow</Button>
        </>
      ) : null}
    </div>
  )
}

/** One bot: what it can ask, what it did, and Remove. */
function BotDetail({ store, bot, view, onClose }: { store: BotStore; bot: BotGuest; view: 'owner' | 'invitee'; onClose: () => void }) {
  const model = botGuestsFor(store)
  useModel(model)
  const [confirm, setConfirm] = useState(false)
  const by = allowedBy(bot, view, ownerOf(store))
  const limits = limitsLine(bot)
  // The chat window opens the chat and, with a root, that conversation's thread.
  const open = (chatBotId: string, root: string | null) => window.kith.island.openBot(`${store.computer.id}/${chatBotId}`, root ?? undefined)
  return (
    <SheetPage>
      <ModalHeader title={bot.name} subtitle={detailSubtitle(bot, view)} />
      <VStack gap={14} className="settings-invite-body">
        <HStack justify="center"><BotGuestAvatar provider={bot.provider} size={84} /></HStack>
        <CardSection title="Can ask" footer={`To change this, remove ${bot.name} and invite it again.`}>
          {bot.agents.map((agent) => (
            <div key={agent} className="settings-row"><Text>{agent}</Text></div>
          ))}
        </CardSection>
        <CardSection footer={limits ?? undefined}>
          <ValueRow label="State"><Text tone="secondary">{stateWord[bot.state]}</Text></ValueRow>
          {bot.joinedAt ? <ValueRow label="Joined"><Text tone="secondary">{ago(bot.joinedAt, Date.now())}</Text></ValueRow> : null}
          <ValueRow label="Last asked"><Text tone="secondary">{bot.lastSeenAt ? ago(bot.lastSeenAt, Date.now()) : 'Not yet'}</Text></ValueRow>
          {by ? <ValueRow label="Allowed by"><Text tone="secondary">{by}</Text></ValueRow> : null}
        </CardSection>
        {view === 'owner' && bot.conversations.length > 0 ? (
          <CardSection title="Conversations">
            {bot.conversations.map((c) => (
              <button key={c.conversation} className="settings-row settings-nav-row" disabled={!c.chatBotId || !store.bots.has(c.chatBotId)} onClick={() => c.chatBotId && open(c.chatBotId, c.threadRoot)}>
                <VStack grow className="settings-stack">
                  <Text truncate>{c.originLabel || c.agent}</Text>
                  <Text size="caption" tone="secondary">{c.agent}{c.lastAt ? ` · ${ago(c.lastAt, Date.now())}` : ''}</Text>
                </VStack>
              </button>
            ))}
          </CardSection>
        ) : null}
        {model.error ? <Text size="footnote" tone="danger">{model.error}</Text> : null}
        <HStack>
          <Spacer />
          <Button kind="secondary" onClick={() => setConfirm(true)} disabled={model.working.has(bot.botId)}>{`Remove ${bot.name}`}</Button>
        </HStack>
      </VStack>
      <Dialog
        open={confirm}
        title={`Remove ${bot.name}?`}
        message={`${bot.name} can't ask your agents any more, and anything it's running stops. Its conversations stay here.${view === 'owner' && bot.ownerName ? ` ${bot.ownerName} keeps their own access.` : ''}`}
        actions={[{ title: 'Remove', destructive: true, action: () => void model.revoke(bot.botId).then(onClose) }]}
        onClose={() => setConfirm(false)}
      />
    </SheetPage>
  )
}

/** The form to invite a bot; `preset` ticks agents up front (from the Share sheet), and Done calls `onDone`. */
export function InviteBotSheet({ store, preset = [], onDone }: { store: BotStore; preset?: string[]; onDone: () => void }) {
  const model = botGuestsFor(store)
  useModel(model)
  const agents = useMemo(() => offeredAgents(store), [store])
  const member = model.mode === 'member'
  const [name, setName] = useState('')
  const [provider, setProvider] = useState<BotProvider>('grok')
  const [ticked, setTicked] = useState<string[]>(preset)
  // The share text lives in this component's state only: it is never stored, logged or sent anywhere else.
  const [made, setMade] = useState<CreatedBotInvite | null>(null)
  useEffect(() => void (model.error = null), [model])
  const ready = name.trim() !== '' && ticked.length > 0
  const create = async () => {
    if (model.busy || !ready) return
    const result = await model.invite({ name: name.trim(), provider, agents: ticked })
    if (result) setMade(result)
  }
  const ownerName = ownerOf(store)
  if (made) {
    const bot = { name: name.trim(), provider }
    return (
      <SheetPage>
        <ModalHeader title="Invite ready" subtitle={botTitle(bot)} />
        <BotInviteResult bot={bot} invite={made} view={member ? 'invitee' : 'owner'} ownerName={ownerName} onDone={onDone} />
      </SheetPage>
    )
  }
  return (
    <SheetPage>
      <ModalHeader title="Invite a bot" subtitle={member ? `On ${store.hostName}` : undefined} />
      <VStack gap={14} className="settings-invite-body">
        <VStack gap={6}>
          <Text size="caption" tone="secondary">Name</Text>
          <input className="settings-input" placeholder="Rex" maxLength={40} value={name} onChange={(e) => setName(e.target.value)} aria-label="Bot name" autoFocus />
          <Text size="footnote" tone="secondary">What it's called here.</Text>
        </VStack>
        <VStack gap={6}>
          <Text size="caption" tone="secondary">From</Text>
          <div style={{ width: 180 }}>
            <SegmentedChoice<BotProvider> selection={provider} options={[{ id: 'grok', label: 'Grok' }, { id: 'other', label: 'Other' }]} onChange={setProvider} />
          </div>
        </VStack>
        <VStack gap={8}>
          <Text size="caption" tone="secondary">What it can ask</Text>
          {agents.length === 0 ? (
            <Text size="callout" tone="secondary">You don't have any agents to share here.</Text>
          ) : (
            agents.map((agent) => (
              <Switch key={agent.id} on={ticked.includes(agent.id)} label={`Let it ask ${agent.name}`} onChange={(on) => setTicked(on ? [...ticked, agent.id] : ticked.filter((a) => a !== agent.id))}>
                <HStack gap={8}>
                  <AgentFace store={store} agent={agent.id} />
                  <Text>{agent.name}</Text>
                </HStack>
              </Switch>
            ))
          )}
          <Text size="footnote" tone="secondary">It gets guest access to what you tick, never more than you have.</Text>
        </VStack>
        {member ? (
          <HStack gap={6}>
            <Icon name="eye" size={12} color="var(--secondary)" />
            <Text size="footnote" tone="secondary">{`${capital(ownerName)} can see what this bot asks, and may need to allow it before it starts.`}</Text>
          </HStack>
        ) : null}
        {model.error ? <Text size="footnote" tone="danger">{model.error}</Text> : null}
        <HStack>
          <Spacer />
          <Button onClick={() => void create()} disabled={!ready || model.busy || agents.length === 0}>{model.busy ? 'Creating…' : 'Create invite'}</Button>
        </HStack>
      </VStack>
    </SheetPage>
  )
}

/** How an invitee names the computer's owner: the host doesn't say a name, so it is the Mac's. */
const ownerOf = (store: BotStore) => `the owner of ${store.hostName}`

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1)

function AgentFace({ store, agent }: { store: BotStore; agent: string }) {
  const bot = store.roster.find((b) => b.hqAgent === agent)
  if (!bot) return <Icon name="bubble.left" size={14} color="var(--secondary)" />
  return <BotAvatar face={avatarImage(bot) ?? DEFAULT_IMAGE} color={bot.avatarColor} id={bot.id} size={24} animated={false} />
}

/** "Invite ready": the host's share text exactly as it wrote it, with Copy and the facts around it. */
function BotInviteResult({ bot, invite, view, ownerName, onDone }: { bot: Pick<BotGuest, 'name' | 'provider'>; invite: CreatedBotInvite; view: 'owner' | 'invitee'; ownerName: string; onDone: () => void }) {
  const [copied, setCopied] = useState(false)
  const provider = providerLabel(bot.provider)
  const who = capital(ownerName)
  return (
    <VStack gap={12} className="settings-invite-body">
      <Text size="callout">{`Paste this into your chat with ${bot.name}${provider ? ` in ${provider}` : ''}.`}</Text>
      <pre className="settings-code settings-bot-share selectable" aria-label="Text to paste into your bot">{invite.share}</pre>
      <HStack>
        <Button onClick={() => { window.kith.app.copy(invite.share); setCopied(true) }}>{copied ? 'Copied' : 'Copy'}</Button>
      </HStack>
      <VStack gap={6}>
        <Fact icon="clock" text={`Expires ${inTime(invite.expiresAt, Date.now())}`} />
        <Fact icon="link" text="Works once, for one bot" />
        <Fact icon="lock.shield" text={`Anyone with this text can join as ${bot.name}. Send it only to your bot.`} />
        {view === 'invitee' ? <Fact icon="eye" text={`${who} can see what ${bot.name} asks`} /> : null}
      </VStack>
      <HStack>
        <Spacer />
        <Button kind="secondary" onClick={onDone}>Done</Button>
      </HStack>
    </VStack>
  )
}

function Fact({ icon, text }: { icon: string; text: string }) {
  return (
    <HStack gap={8} align="flex-start">
      <Icon name={icon} size={12} color="var(--secondary)" />
      <Text size="callout" tone="secondary">{text}</Text>
    </HStack>
  )
}

/** The owner's "Your bots" block under People: their own bots, an Invite a bot button, and what the bots are for. */
export function OwnBotsSection({ store, bots, onInvite }: { store: BotStore; bots: BotGuest[]; onInvite: () => void }) {
  return (
    <CardSection title="Your bots" footer="Bots from Grok and other apps can ask the agents you pick. They never act as you.">
      <BotList store={store} bots={bots} view="owner" />
      {bots.length > 0 ? <BotsCapRow store={store} /> : null}
      <div className="settings-row">
        <Button kind="secondary" onClick={onInvite}>Invite a bot</Button>
      </div>
    </CardSection>
  )
}

/** "Monthly cap": what your bots may spend together on every agent before they stop and you get a card; off by default. */
function BotsCapRow({ store }: { store: BotStore }) {
  const [state, setState] = useState<BotsCap | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    let live = true
    void store.ready().then((c) => c.botsCap()).then(
      (s) => { if (live) setState(s) },
      (e: unknown) => { if (live && !isUnsupportedHost(e)) setError(e instanceof Error ? e.message : String(e)) },
    )
    return () => { live = false }
  }, [store])
  if (!state) return error ? <div className="settings-row"><Text size="footnote" tone="danger">{error}</Text></div> : null
  const set = (cap: number | null) => {
    setBusy(true)
    setError(null)
    void store.ready().then((c) => c.setBotsCap(cap)).then(setState, (e: unknown) => setError(e instanceof Error ? e.message : String(e))).finally(() => setBusy(false))
  }
  const spent = state.cap ? ofLimit(state.spent, state.cap, true) : `${money(state.spent, true)} this month`
  return (
    <div className="settings-row">
      <VStack grow className="settings-stack">
        <Text truncate>Monthly cap</Text>
        <Text size="caption" tone={error ? 'danger' : 'secondary'}>{error ?? `${spent} · all agents, at API prices`}</Text>
      </VStack>
      <BudgetPicker name="your bots" budget={state.cap} busy={busy} set={set} />
    </div>
  )
}

/** The invitee's Settings page: one section per computer they are a member of, each with their bots there. */
export function MyBotsPage() {
  const app = useApp()
  const stores = [...app.stores.values()].filter((s) => botGuestsFor(s).mode === 'member')
  const models = stores.map(botGuestsFor)
  useModels(models)
  const [inviting, setInviting] = useState<BotStore | null>(null)
  useEffect(() => {
    for (const m of models) void m.load()
  }, [models.length]) // eslint-disable-line react-hooks/exhaustive-deps
  const shown = stores.filter((s) => botGuestsFor(s).supported !== false)
  if (shown.length === 0) return <CardForm><Text tone="secondary">Nothing here yet.</Text></CardForm>
  return (
    <CardForm>
      {shown.map((store) => {
        const model = botGuestsFor(store)
        return (
          <CardSection key={store.computer.id} title={shown.length > 1 ? store.hostName : 'Your bots'} footer={model.error ?? `${capital(ownerOf(store))} can see what your bots ask.`}>
            {model.bots.length === 0 ? <div className="settings-row"><Text tone="secondary">No bots yet.</Text></div> : <BotList store={store} bots={model.bots} view="invitee" />}
            <div className="settings-row">
              <Button kind="secondary" onClick={() => setInviting(store)}>Invite a bot</Button>
            </div>
          </CardSection>
        )
      })}
      <Sheet className="settings-sheet" open={inviting !== null} onClose={() => setInviting(null)} width={480}>
        {inviting ? <InviteBotSheet store={inviting} onDone={() => setInviting(null)} /> : null}
      </Sheet>
    </CardForm>
  )
}
