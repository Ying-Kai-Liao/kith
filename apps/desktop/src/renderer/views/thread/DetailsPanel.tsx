import type { ReactNode } from 'react'
import { isGroup, isWorking, type Bot } from '@shared/models'
import { AgentIcon } from '../../components/AgentIcon'
import { BotAvatar, GroupAvatar } from '../../components/Avatar'
import { IconButton, TabChoice } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import type { KithInfoStore } from '../../store/kith-info-store'
import { BotSettingsPanel } from '../bots/BotEditorView'
import { AccessTab } from '../bots/kith/AccessTab'
import { KithDetails, KithHeader } from '../bots/kith/KithDetails'
import { useKithInfo } from '../bots/kith/use-kith'
import { MemoryCard } from '../bots/MemoryView'
import { RoutinesView } from '../routines/RoutinesView'
import { MacChatButton } from './MacChatButton'
import { BotGuestAvatar } from '../../components/kith'
import { botGuestsFor } from '../../store/bot-guests-store'
import { useModel } from '../../lib/observable'
import { botRowLine } from '@shared/room-join'
import { roomBotsOf } from './room-bots'
import { BotTag } from './Tags'
import './conversation.css'
import '../bots/bots.css'
import './details-panel.css'
import './agent-sheet.css'

export type DetailsTab = 'details' | 'memory' | 'access' | 'settings'

const TABS: { id: DetailsTab; label: string }[] = [
  { id: 'details', label: 'Details' },
  { id: 'memory', label: 'Memory' },
  { id: 'access', label: 'Access' },
  { id: 'settings', label: 'Settings' },
]

/** The inspector beside a conversation (Grok Bot's): who the bot is, then its details, memory and settings. */
export function DetailsPanel({ botId, routineId, callBlocked, onCall, tab, setTab, editGroup, addMember, share, close }: {
  botId: string
  routineId: string | null
  /** Why a call can't start now (offline, no voice); null when it can. */
  callBlocked: string | null
  /** Starts a call in this chat; null while one is running. */
  onCall: (() => void) | null
  tab: DetailsTab
  setTab: (tab: DetailsTab) => void
  editGroup: () => void
  /** Opens the Add sheet (agents and bots) for this group. */
  addMember: () => void
  share: () => void
  close: () => void
}) {
  const store = useStore()
  const bot = store.bots.get(botId) ?? null
  const group = !!bot && isGroup(bot)
  const kith = useKithInfo(botId, true)
  // Access is the owner's page on a Kith agent whose host knows kith details; every other bot keeps its tabs.
  const hasAccess = !!bot?.hqAgent && store.isOwner && !!kith && !kith.unsupported
  const tabs = hasAccess ? TABS : TABS.filter((t) => t.id !== 'access')
  const shownTab = tabs.some((t) => t.id === tab) ? tab : 'details'
  return (
    <div className="details-panel">
      <div className="details-drag" />
      <div className="details-actions">
        {bot && !group ? (
          <button className="details-template press" title="Create template" onClick={share}>
            <Icon name="square.and.arrow.up" size={12} weight="medium" /> Create template
          </button>
        ) : <span />}
        <MacChatButton className="ringed" title="Close details" icon="xmark" onClick={close} />
      </div>
      <div className="details-scroll">
        {bot ? <Identity bot={bot} kith={kith} /> : null}
        {bot ? (
          <div className="details-call">
            <button className="call-capsule" disabled={!onCall || !!callBlocked} title={callBlocked ?? (onCall ? 'Start a voice call' : 'A call is running')} onClick={() => onCall?.()}>
              <Icon name="phone.fill" size={12} weight="medium" /> {onCall ? 'Call' : 'In call'}
            </button>
          </div>
        ) : null}
        {bot && group ? (
          <GroupMembersList group={bot} editGroup={editGroup} addMember={addMember} />
        ) : (
          <>
            <TabChoice label="Conversation details" selection={shownTab} options={tabs} onChange={setTab} />
            <div key={shownTab} className="details-tab fade-in">
              {shownTab === 'details' ? (
                <DetailsBody botId={botId} routineId={routineId} kith={kith} />
              ) : shownTab === 'memory' ? (
                <MemoryCard botId={botId} />
              ) : shownTab === 'access' && kith && bot?.hqAgent ? (
                <AccessTab agent={bot.hqAgent} kith={kith} />
              ) : (
                <BotSettingsPanel botId={botId} part="rarely" />
              )}
            </div>
          </>
        )}
      </div>
    </div>
  )
}

/** Details: about, cost, schedules and routines, projects, harness, skills. An hq bot's Kith details lead; without them the form's About card and the routines stand alone. */
function DetailsBody({ botId, routineId, kith }: { botId: string; routineId: string | null; kith: KithInfoStore | null }) {
  const routines = (extra?: ReactNode) => <RoutinesView botId={botId} initialId={routineId} extra={extra} />
  if (kith && !kith.unsupported && kith.info) {
    return <KithDetails kith={kith} about={<BotSettingsPanel botId={botId} part="often" bare />} routines={routines} />
  }
  return (
    <>
      {kith ? <KithDetails kith={kith} /> : null}
      <BotSettingsPanel botId={botId} part="often" />
      {routines()}
    </>
  )
}

/** The bot's avatar and name, and what runs it (a group: how many bots it has). */
function Identity({ bot, kith }: { bot: Bot; kith: KithInfoStore | null }) {
  const store = useStore()
  const group = isGroup(bot)
  return (
    <div className="details-identity">
      {group ? <GroupAvatar members={store.members(bot)} size={64} /> : <BotAvatar bot={bot} size={64} />}
      <span className="details-name">{bot.name}</span>
      <span className="details-subtitle">
        {group ? (
          `${bot.members.length} ${bot.members.length === 1 ? 'bot' : 'bots'}`
        ) : (
          <>
            <AgentIcon registry={store.hello?.backends.find((b) => b.id === bot.backend)?.registry ?? bot.backend} size={12} />
            {store.backendName(bot.backend)}
            {bot.model ? ` · ${bot.model}` : null}
            {kith && bot.hqAgent ? <KithHeader agent={bot.hqAgent} kith={kith} /> : null}
          </>
        )}
      </span>
    </div>
  )
}

/** A group's agents and outside bots (Grok Bot's member list); clicking an agent opens its own chat. */
function GroupMembersList({ group, editGroup, addMember }: { group: Bot; editGroup: () => void; addMember: () => void }) {
  const store = useStore()
  // The owner's bot list names the outside bots and says where each runs.
  const guests = useModel(botGuestsFor(store))
  const outside = roomBotsOf(store, group)
  const total = group.members.length
  return (
    <div className="details-members">
      <div className="details-members-head">
        <Text size="caption" weight="semibold" tone="secondary">AGENTS AND BOTS</Text>
        {store.isOwner ? <button className="press details-members-add" onClick={addMember}>Add</button> : null}
      </div>
      {store.members(group).map((bot) => (
        <div key={bot.id} className="member-row removable">
          <button className="member-open press" title={`Open ${bot.name}'s own chat`} onClick={() => store.setSelection(bot.id)}>
            <BotAvatar bot={bot} size={32} />
            <VStack as="span" gap={1} grow>
              <span className="member-name">{bot.name}</span>
              {isWorking(bot) ? <Text size="compactSecondary" tone="secondary" className="member-name">{bot.activity || 'Working…'}</Text> : null}
            </VStack>
          </button>
          {total > 1 ? <IconButton title={`Remove ${bot.name} from the group`} icon="xmark" onClick={() => store.removeMember(group, bot.id)} /> : null}
        </div>
      ))}
      {outside.map((bot) => (
        <div key={bot.id} className="member-row removable bot-member">
          <div className="member-open">
            <BotGuestAvatar provider={bot.provider} size={32} />
            <VStack as="span" gap={1} grow>
              <span className="member-name"><span className="bot-name">{bot.name}</span><BotTag /></span>
              <Text size="compactSecondary" tone="secondary">{botRowLine(guests.bots.find((g) => g.botId === bot.id) ?? { provider: bot.provider, ownerName: null, ownerPerson: null })}</Text>
            </VStack>
          </div>
          {store.isOwner ? <button className="press bot-member-remove" onClick={() => store.removeMember(group, bot.id)}>Remove</button> : <span />}
        </div>
      ))}
      <button className="member-row press" onClick={editGroup}>
        <span className="member-add-icon">
          <Icon name="square.and.pencil" size={14} color="var(--secondary)" />
        </span>
        <Text tone="secondary" className="member-name">Edit group</Text>
      </button>
    </div>
  )
}
