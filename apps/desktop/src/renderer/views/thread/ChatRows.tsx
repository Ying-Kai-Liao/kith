import { SharedFileView } from './SharedImage'
import { memo, useEffect, useRef, useState } from 'react'
import { isGroup, isWorking, needsInput, type Bot, type Entry } from '@shared/models'
import { BotAvatar } from '../../components/Avatar'
import { BotAvatar as KithBot } from '../../components/kith'
import { avatarImage } from '@shared/crew'
import { Hairline, IconButton, Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { AnchoredMenu, useContextMenu, type MenuItem, type ReactionPick } from '../../components/Overlay'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { ThinkingOrb } from '../../components/ThinkingOrb'
import { useStore as useLiveStore, useStoreRef as useStore } from '../../store/context'
import { QUICK_REACTIONS, type BotStore } from '../../store/bot-store'
import { AttachmentList } from './Attachments'
import { relativeTime } from './chat-items'
import { MarkdownText } from './Markdown'
import { ownerNameFrom, waitingFor } from '@shared/mac-role'
import { PermissionCard, type HeldCardProps } from './PermissionCard'
import { LimitCard } from './LimitCard'
import { limitOf } from '@shared/usage-limits'
import { AccessCard, KeyCard } from './SecretCards'
import { isSecretCard } from '@shared/secret-card'
import { confirmMessage, confirmTitle, heldIcon, heldRowOf, stoppedDivider } from '@shared/held-run'
import { ConnectionRequestCard } from '../marketplace/ConnectionRequestCard'
import { HandoffNotice } from './HandoffNotice'
import { AnsweredBy } from './AnsweredBy'
import { contextLine } from './context-hint'
import { groupReactions } from './reaction-groups'
import { FoldedPill } from './FoldedCard'
import { foldOf } from './fold-card'
import { callLineSuffix } from './call-line'
import { BotGuestAvatar, ToolRow, pairOf } from '../../components/kith'
import { toolRowOf, VISIBLE_STEPS } from '@shared/thread-feed'
import { botGuestSender } from '@shared/bot-guest-chat'
import { queuedLine } from '@shared/thread-writer'
import { quoteOf, quotesOf } from './reply-quote'
import { richCardOf, type RichCard } from '@shared/rich-card'
import { NewsCard, PlanCard, ScheduleCard } from './RichCards'
import { lateOf } from '@shared/conversation'
import { BotTag, LateTag } from './Tags'
import { topicMenuItems } from './TopicPrompt'

/** The first line of the message that opened the run a reply belongs to, for a late reply to quote; null when unknown. */
export function lateQuoteOf(store: BotStore, entry: Entry): string | null {
  if (entry.data.late !== true) return null
  const opening = store.conversations.get(entry.data.conversation)?.opening
  return opening ? quoteOf(store.allEntries(entry.botId).find((e) => e.id === opening)?.data.text) : null
}

export function reactionPick(store: BotStore, entry: Entry): ReactionPick {
  return { emoji: QUICK_REACTIONS, chosen: entry.data.reactions ?? [], toggle: (e) => store.react(entry, e) }
}

/** One chat-visible entry in the flat chat, with what can be done to it. */
export const ChatRow = memo(function ChatRow({ entry, groupStart, chat, openTrace, lateQuote = null }: {
  entry: Entry
  groupStart: boolean
  chat: Bot | null
  openTrace: () => void
  /** A late reply's quote of the message that opened its run (the row can't follow the store itself). */
  lateQuote?: string | null
}) {
  const store = useStore()
  // Reply is a quote above the composer; the host has no reply link.
  const quote = quoteOf(entry.data.text)
  const reply = quote && !chat?.botGuest ? () => quotesOf(store).set(entry.botId, quote) : undefined
  const guestSender = botGuestSender(chat, entry)
  const botPost = entry.kind === 'user' ? botPostOf(chat, entry, guestSender?.name) : null
  switch (entry.kind) {
    case 'user':
      // An outside bot's own message (in its read-only chat, or in a group it was added to): the other party, on the
      // left with its picture, its name in Inter and a Bot capsule.
      if (botPost) {
        const late = lateOf(entry.data)
        return (
          <VStack align="flex-start" gap={4} className={groupStart || late ? 'chat-row group-start' : 'chat-row'}>
            <HStack gap={8}>
              <BotGuestAvatar provider={botPost.provider} size={24} />
              <span className="bot-name">{botPost.name}</span>
              <BotTag />
              {late ? <LateTag /> : null}
            </HStack>
            <VStack gap={4} className="chat-row-indent">
              {late ? <LateBubble entry={entry} reason={late.reason} quote={lateQuote} reply={reply} /> : <GuestBotBubble entry={entry} reply={reply} />}
              <ReactionsRow entry={entry} />
            </VStack>
          </VStack>
        )
      }
      return (
        <VStack align="flex-end" gap={4} className={groupStart ? 'chat-row group-start' : 'chat-row'}>
          <UserBubble entry={entry} botWorking={!!chat && isWorking(chat)} reply={reply} />
          <ReactionsRow entry={entry} />
        </VStack>
      )
    case 'agent': {
      const card = richCardOf(entry.data)
      // A late reply always names its author: it sits outside the flow it arrived in.
      const late = lateOf(entry.data)
      return (
        <VStack align="flex-start" gap={4} className={groupStart || late ? 'chat-row group-start' : 'chat-row'}>
          {groupStart || late ? <AuthorLabel botId={entry.data.author ?? entry.botId}>{late ? <LateTag /> : null}</AuthorLabel> : null}
          {card ? <AgentCard entry={entry} card={card} openTrace={openTrace} reply={reply} /> : late ? <LateBubble entry={entry} reason={late.reason} quote={lateQuote} reply={reply} /> : <AgentBubble entry={entry} openTrace={openTrace} reply={reply} />}
          <VStack gap={4} className={chat && isGroup(chat) ? 'chat-row-indent' : undefined}>
            <AnsweredBy answeredBy={entry.data.answeredBy} />
            <ReactionsRow entry={entry} />
          </VStack>
        </VStack>
      )
    }
    case 'permission':
      return (
        <VStack gap={4} className="chat-row group-start">
          {chat && isGroup(chat) ? <AuthorLabel botId={entry.data.author} /> : null}
          <LivePermission entry={entry} />
        </VStack>
      )
    default:
      if (entry.data.connectionRequest) return <div className="chat-row group-start"><ConnectionRequestCard entry={entry} /></div>
      if (entry.data.handoff) return <div className="chat-row notice"><HandoffNotice entry={entry} card={entry.data.handoff} /></div>
      // A routine's "Created routine" notice is its Schedule card.
      if (richCardOf(entry.data)?.type === 'schedule') return <div className="chat-row group-start"><div className="agent-inset"><ScheduleCard entry={entry} card={richCardOf(entry.data)!} /></div></div>
      // A run's plan is one card that changes in place; an app without it shows the notice's text twin.
      if (richCardOf(entry.data)?.type === 'plan') return <div className="chat-row group-start"><div className="agent-inset"><PlanCard entry={entry} card={richCardOf(entry.data)!} /></div></div>
      // Any notice that roots a thread (a bot guest's conversation) opens it like a message does.
      return <div className="chat-row notice"><NoticeRow entry={entry} /></div>
  }
})

/** An agent message drawn as its card. A brief's lead and tail stay the agent's words, as bubbles around it. */
function AgentCard({ entry, card, openTrace, reply }: { entry: Entry; card: RichCard; openTrace: () => void; reply?: () => void }) {
  if (card.type === 'schedule') return <div className="agent-inset"><ScheduleCard entry={entry} card={card} /></div>
  const words = (key: 'lead' | 'tail') => (typeof card.snapshot?.[key] === 'string' ? (card.snapshot[key] as string).trim() : '')
  const bubble = (text: string) => (text ? <AgentBubble entry={{ ...entry, data: { ...entry.data, text } }} openTrace={openTrace} reply={reply} /> : null)
  return (
    <>
      {bubble(words('lead'))}
      <div className="agent-inset"><NewsCard entry={entry} card={card} /></div>
      {bubble(words('tail'))}
    </>
  )
}

/** Under a tapped reply, at its bottom right: the time, then a 13 pt ring and "N% context" when the host sent a percent. */
function ContextLine({ entry }: { entry: Entry }) {
  const line = contextLine(relativeTime.time(entry.createdAt), entry.data.contextPercent)
  const r = 5.5
  const c = 2 * Math.PI * r
  return (
    <div className="context-line">
      <span className="context-line-time">{line.time}</span>
      {line.fill !== null && line.label ? (
        <>
          <svg className="context-ring" width={13} height={13} viewBox="0 0 13 13" aria-hidden>
            <circle cx="6.5" cy="6.5" r={r} className="context-ring-track" />
            <circle cx="6.5" cy="6.5" r={r} className="context-ring-arc" strokeDasharray={`${line.fill * c} ${c}`} transform="rotate(-90 6.5 6.5)" />
          </svg>
          <span className="context-line-label">{line.label}</span>
        </>
      ) : null}
    </div>
  )
}

/** Follows the store: the card shows a spinner while its answer is on its way. */
function LivePermission({ entry }: { entry: Entry }) {
  const store = useLiveStore()
  const [open, setOpen] = useState(false)
  const fold = foldOf(entry)
  // An approved card that still holds its old run keeps its Stop row, so it stays a card.
  const folded = fold && !(entry.data.held || store.stoppedRuns.get(entry.id))
  if (folded) {
    return (
      <div className="agent-inset">
        <FoldedPill fold={fold} time={relativeTime.time(entry.createdAt)} open={open} toggle={() => setOpen((o) => !o)} />
        {open ? <div className="folded-card-body"><OpenCard entry={entry} /></div> : null}
      </div>
    )
  }
  return <OpenCard entry={entry} />
}

/** The full card for an entry, pending or answered. */
export function OpenCard({ entry }: { entry: Entry }) {
  const store = useLiveStore()
  const ownerName = ownerNameFrom(store.bots.get(entry.botId)?.activity)
  // A key request is its own card; an entry without data.card (or an old one) is the approval card as before.
  if (isSecretCard(entry)) {
    const author = store.bots.get(entry.data.author ?? entry.botId)
    const common = { entry, agent: author?.name ?? entry.data.agent ?? 'The agent', pair: pairOf(author?.avatarColor), answering: store.answering.get(entry.id) ?? null, respond: (o: string) => store.respond(entry, o) }
    return entry.data.card === 'account' ? <AccessCard {...common} /> : <KeyCard {...common} save={(account, values) => store.provideSecret(entry, account, values).then(() => undefined)} />
  }
  if (limitOf(entry)) {
    const author = store.bots.get(entry.data.author ?? entry.botId)
    const at = new Date(entry.createdAt)
    return (
      <div className="agent-inset">
        <LimitCard
          entry={entry}
          pair={pairOf(author?.avatarColor)}
          answering={store.answering.get(entry.id) ?? null}
          respond={(o) => store.respond(entry, o)}
          waiting={waitingFor(entry, ownerName, store.isOwner)}
          leading={author ? <BotAvatar bot={author} size={18} animated={false} /> : undefined}
          time={`${String(at.getHours()).padStart(2, '0')}:${String(at.getMinutes()).padStart(2, '0')}`}
        />
      </div>
    )
  }
  return <PermissionCard entry={entry} hostName={store.hostName} answering={store.answering.get(entry.id) ?? null} respond={(o) => store.respond(entry, o)} waiting={waitingFor(entry, ownerName, store.isOwner)} pair={pairOf(store.bots.get(entry.data.author ?? entry.botId)?.avatarColor)} held={heldFor(entry, store, ownerName)} />
}

/** An approved card's held row (an old run holds its session): Stop is the owner's, others read who can stop it. */
function heldFor(entry: Entry, store: BotStore, ownerName: string | null): HeldCardProps | null {
  const held = entry.data.held
  const stopped = store.stoppedRuns.get(entry.id)
  const divider = stopped ? { text: stoppedDivider(stopped.kind, stopped.at), icon: heldIcon(stopped.kind) } : null
  if (!held || entry.data.status !== 'answered') return divider ? { model: null, confirm: { title: confirmTitle, message: '' }, stop: () => {}, stopped: divider } : null
  const agent = entry.data.agent || 'the agent'
  return {
    model: heldRowOf({ held, owner: store.isOwner, ownerName, phase: store.holderPhases.get(held.session) }),
    confirm: { title: confirmTitle, message: confirmMessage({ held, mac: store.hostName, agent }) },
    stop: () => store.stopHolder(entry),
    stopped: divider,
  }
}

/** Who wrote a message: the bot at 24 (no tile, soft shadow) and its name in Nunito 800. */
export function AuthorLabel({ botId, children }: { botId?: string | null; children?: React.ReactNode }) {
  const store = useStore()
  const bot = botId ? store.bots.get(botId) : undefined
  return (
    <HStack gap={8}>
      {bot ? <KithBot face={isGroup(bot) ? 'group' : (avatarImage(bot) ?? 'group')} color={bot.avatarColor} id={bot.id} size={24} animated={false} /> : null}
      <span className="kith-text-agent-name author-name">{store.authorName(botId)}</span>
      {children}
    </HStack>
  )
}

/** A message's text with a right-click menu (with quick reactions on top). */
export function Bubble({ className, items, reactions, children }: { className: string; items: () => MenuItem[]; reactions?: ReactionPick; children: React.ReactNode }) {
  const menu = useContextMenu()
  return (
    <>
      <div className={className} onContextMenu={menu.onContextMenu}>
        {children}
      </div>
      <AnchoredMenu open={menu.open} onClose={menu.close} point={menu.point} items={items} reactions={reactions} />
    </>
  )
}

/** The centered time label between runs of messages. */
export function TimeSeparator({ date }: { date: number }) {
  return <Text as="div" size="footnote" tone="tertiary" className="time-separator">{relativeTime.separator(date)}</Text>
}

const fullDate = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' })

export function UserBubble({ entry, botWorking, reply }: { entry: Entry; botWorking: boolean; reply?: () => void }) {
  const store = useStore()
  const [hovering, setHovering] = useState(false)
  const status = entry.data.status
  const copy = () => entry.data.text && window.kith.app.copy(entry.data.text)
  return (
    <div className="message user" title={fullDate.format(entry.createdAt)} onMouseEnter={() => setHovering(true)} onMouseLeave={() => setHovering(false)}>
      <div className="message-body">
        {entry.data.attachments?.length ? <AttachmentList attachments={entry.data.attachments} botId={entry.botId} /> : null}
        {entry.data.text ? (
          <Bubble
            className="bubble user selectable"
            reactions={reactionPick(store, entry)}
            items={() => [
              ...(reply ? [{ title: 'Reply', icon: 'arrowshape.turn.up.left', action: reply }] : []),
              { title: 'Copy', icon: 'square.on.square', action: copy },
              ...(status === 'queued' ? [{ title: 'Unsend', icon: 'arrow.uturn.backward', action: () => store.unsend(entry) }] : []),
              ...topicMenuItems(store, entry),
            ]}
          >
            {entry.data.text}
          </Bubble>
        ) : null}
        {status && status !== 'sent' ? <UserStatus entry={entry} botWorking={botWorking} hovering={hovering} /> : null}
        {/* Beside the bubble, inside its hover region: showing them never changes the height. */}
        <MessageActions className="leading" visible={hovering} reactions={reactionPick(store, entry)} reply={reply} copy={copy} />
      </div>
    </div>
  )
}

function UserStatus({ entry, botWorking, hovering }: { entry: Entry; botWorking: boolean; hovering: boolean }) {
  const store = useStore()
  switch (entry.data.status) {
    case 'sending':
      return <Text size="caption2" tone="tertiary">Sending…</Text>
    case 'queued':
      return (
        <HStack as="span" gap={2}>
          <Text size="caption2" tone="tertiary">{queuedLine(entry.data.waitsFor, botWorking)}</Text>
          <IconButton title="Unsend" icon="arrow.uturn.backward" size={24} onClick={() => store.unsend(entry)} />
        </HStack>
      )
    case 'failed':
      return (
        <HStack as="span" gap={2}>
          <Text size="caption2" weight="semibold" tone="danger" className="user-status-error">Failed to send</Text>
          <IconButton title="Resend" icon="arrow.clockwise" size={24} onClick={() => store.retry(entry)} />
          <IconButton title="Delete" icon="trash" size={24} onClick={() => store.discard(entry)} />
        </HStack>
      )
    case 'cancelled':
      return <Text size="caption2" tone="tertiary">Not sent</Text>
    default:
      return <Text size="caption" tone="tertiary" className={hovering ? 'user-status-time shown' : 'user-status-time'}>{relativeTime.time(entry.createdAt)}</Text>
  }
}

/** An outside bot's message: an agent-style bubble on the left (reply and copy, no queue or unsend). */
function GuestBotBubble({ entry, reply }: { entry: Entry; reply?: () => void }) {
  const store = useStore()
  const copy = () => entry.data.text && window.kith.app.copy(entry.data.text)
  return (
    <div className="message agent" title={fullDate.format(entry.createdAt)}>
      <div className="message-body">
        <Bubble
          className="bubble agent selectable"
          reactions={reactionPick(store, entry)}
          items={() => [
            ...(reply ? [{ title: 'Reply', icon: 'arrowshape.turn.up.left', action: reply }] : []),
            { title: 'Copy', icon: 'square.on.square', action: copy },
          ]}
        >
          <MarkdownText text={entry.data.text ?? ''} />
        </Bubble>
      </div>
    </div>
  )
}

export function AgentBubble({ entry, openTrace, reply }: { entry: Entry; openTrace: () => void; reply?: () => void }) {
  const store = useStore()
  const [hovering, setHovering] = useState(false)
  const [line, setLine] = useState(false)
  const copy = () => entry.data.text && window.kith.app.copy(entry.data.text)
  // Scrolling the chat takes the line away again.
  useEffect(() => {
    if (!line) return
    const hide = () => setLine(false)
    window.addEventListener('scroll', hide, true)
    return () => window.removeEventListener('scroll', hide, true)
  }, [line])
  // A tap on the reply itself; links, buttons and a text selection keep their own meaning.
  const tap = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest('a, button, input, textarea, summary, [role="button"]')) return
    if (window.getSelection()?.toString()) return
    setLine((v) => !v)
  }
  return (
    <div className="message agent" title={fullDate.format(entry.createdAt)} onMouseEnter={() => setHovering(true)} onMouseLeave={() => setHovering(false)}>
      <div className="message-body" onClick={tap}>
        <Bubble
          className={entry.data.files?.length ? "file-message" : "bubble agent"}
          reactions={reactionPick(store, entry)}
          items={() => [
            ...(reply ? [{ title: 'Reply', icon: 'arrowshape.turn.up.left', action: reply }] : []),
            { title: 'Copy', icon: 'square.on.square', action: copy },
            { title: 'Show what it did', icon: 'list.bullet', action: openTrace },
            ...topicMenuItems(store, entry),
          ]}
        >
          {entry.data.files?.length ? <div className="file-message-cards">{entry.data.files.map((file) => <SharedFileView key={file.id} entry={entry} file={file} />)}</div> : <MarkdownText text={entry.data.text ?? ''} />}
        </Bubble>
        {line ? <ContextLine entry={entry} /> : null}
        <MessageActions className="trailing" visible={hovering} reactions={reactionPick(store, entry)} reply={reply} trace={openTrace} copy={copy} />
      </div>
    </div>
  )
}

export function NoticeRow({ entry }: { entry: Entry }) {
  const text = entry.data.text ?? ''
  if (ROOM_NOTICES.has(entry.data.notice ?? '')) return <RoomNotice text={text} bot={entry.data.bot} />
  const call = callLineSuffix(entry.data, entry.createdAt)
  if (call !== null) {
    return (
      <HStack justify="center" gap={6}>
        <Icon name="waveform" size={10} color="var(--secondary)" />
        <Text size="footnote" tone="secondary" tabular>{`${text}${call}`}</Text>
      </HStack>
    )
  }
  switch (entry.data.style) {
    case 'divider':
      return (
        <HStack gap={10} className="chat-notice-divider">
          <Hairline grow />
          <Text size="caption2" tone="tertiary" className="chat-notice-divider-text">{text}</Text>
          <Hairline grow />
        </HStack>
      )
    case 'error':
      return (
        <HStack align="flex-start" gap={8} className="chat-notice-error">
          <Icon name="exclamationmark.triangle.fill" size={10} color="var(--danger)" />
          <Text size="footnote" tone="text" className="selectable">{text}</Text>
        </HStack>
      )
    default:
      return <Text as="div" size="footnote" tone="secondary" className="chat-notice">{text}</Text>
  }
}

/**
 * The bot's live activity line: a turning orb, what it's doing and for how long. Clicking it
 * unfolds what the bot is thinking right now.
 */
export function WorkingIndicator({ bot, thinking }: { bot: Bot; thinking: string | null }) {
  const [expanded, setExpanded] = useState(false)
  const waiting = needsInput(bot)
  return (
    <HStack gap={10} className="agent-inset">
      <button className="working" disabled={!thinking} onClick={() => setExpanded((e) => !e)} aria-expanded={thinking ? expanded : undefined}>
        <HStack as="span" gap={8}>
          <ThinkingOrb state={waiting ? 'listening' : 'working'} size={16} color={waiting ? 'var(--warning)' : 'var(--secondary)'} />
          <Text size="subheadline" tone={waiting ? 'attention' : 'secondary'} truncate>{bot.activity || 'Working…'}</Text>
          {bot.startedAt ? <Timer since={bot.startedAt} /> : null}
          {thinking ? (
            <HStack as="span" align="stretch" className={expanded ? 'working-chevron expanded' : 'working-chevron'}>
              <Icon name="chevron.down" size={10} weight="semibold" color="var(--tertiary)" />
            </HStack>
          ) : null}
        </HStack>
        {expanded && thinking ? (
          <div className="selectable thinking" ref={(el) => {
            if (el) el.scrollTop = el.scrollHeight
          }}>
            {thinking}
          </div>
        ) : null}
      </button>
    </HStack>
  )
}

/** `Text(date, style: .timer)`: 0:42, 12:05, 1:02:33. */
function Timer({ since }: { since: number }) {
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [])
  const s = Math.max(0, Math.floor((now - since) / 1000))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = String(s % 60).padStart(2, '0')
  return <Text size="footnote" tone="tertiary" tabular>{h ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`}</Text>
}

/** The user's reactions under a message; clicking one takes it back. */
export function ReactionsRow({ entry }: { entry: Entry }) {
  const store = useStore()
  const groups = groupReactions(entry.data.reactions)
  if (!groups.length) return null
  return (
    <HStack align="stretch" gap={4}>
      {groups.map(({ emoji, count, mine }) => (
        <button key={emoji} className={mine ? 'press reaction-chip mine' : 'press reaction-chip'} title={`Remove ${emoji}`} aria-label={`Remove reaction ${emoji}`} onClick={() => store.react(entry, emoji)}>
          <Text size="body">{emoji}</Text>
          <span className="reaction-count">{count}</span>
        </button>
      ))}
    </HStack>
  )
}

/** Compact actions beside the bubble: a reaction strip, reply, and more (copy, what it did). */
function MessageActions({ visible, className, reactions, reply, trace, copy }: {
  visible: boolean
  className: string
  reactions: ReactionPick
  reply?: () => void
  trace?: () => void
  copy: () => void
}) {
  const [menu, setMenu] = useState<'reactions' | 'more' | null>(null)
  const smile = useRef<HTMLButtonElement>(null)
  const more = useRef<HTMLButtonElement>(null)
  const shown = visible || menu !== null
  return (
    <div className={`message-actions ${className} ${shown ? 'shown' : ''}`} aria-hidden={!shown}>
      <button ref={smile} className="message-action" title="Add reaction" aria-label="Add reaction" onClick={() => setMenu('reactions')}>
        <Icon name="face.smiling" size={14} />
      </button>
      {reply ? <IconButton title="Reply" icon="arrowshape.turn.up.left" size={26} onClick={reply} /> : null}
      <button ref={more} className="message-action" title="More message actions" aria-label="More message actions" onClick={() => setMenu('more')}>
        <Icon name="ellipsis" size={14} />
      </button>
      <AnchoredMenu open={menu === 'reactions'} onClose={() => setMenu(null)} anchor={smile} items={() => []} reactions={reactions} />
      <AnchoredMenu
        open={menu === 'more'}
        onClose={() => setMenu(null)}
        anchor={more}
        items={() => [{ title: 'Copy', icon: 'square.on.square', action: copy }, ...(trace ? [{ title: 'Show what it did', icon: 'list.bullet', action: trace }] : [])]}
      />
    </div>
  )
}

/** A tool call as one row in the chat: "Edited kith-sessions.py  +6 -2"; it opens the command and output. */
export function ToolRowItem({ entry }: { entry: Entry }) {
  const m = toolRowOf(entry)
  return (
    <div className="chat-tool">
      <ToolRow verb={m.verb} object={m.object} result={m.result} state={m.state} detail={m.command || m.output ? { command: m.command, output: m.output } : undefined} />
    </div>
  )
}

/** The steps of a long run folded away; it opens the full conversation. */
export function EarlierSteps({ count, open }: { count: number; open: () => void }) {
  return (
    <button className="chat-earlier" onClick={open}>
      {count === 1 ? '1 earlier step' : `${count} earlier steps`}
    </button>
  )
}

/**
 * A run of tool calls as one quiet row: "3 steps", "Working · 3 steps", "3 steps · 1 failed". It opens the steps
 * inline; the newest `VISIBLE_STEPS` show and "N earlier steps" above them opens the full trace.
 */
export function StepsRow({ entries, openTrace }: { entries: Entry[]; openTrace: () => void }) {
  const [open, setOpen] = useState(false)
  const states = entries.map((e) => toolRowOf(e).state)
  const failed = states.filter((s) => s === 'error').length
  const working = states.includes('running')
  const n = entries.length
  const count = n === 1 ? '1 step' : `${n} steps`
  const earlier = n - VISIBLE_STEPS
  return (
    <div className="chat-steps">
      <button className="chat-earlier chat-steps-toggle" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        {working ? <Spinner size={12} /> : null}
        <span>{working ? `Working · ${count}` : count}</span>
        {failed ? <span className="chat-steps-failed"> · {failed} failed</span> : null}
        <Icon name="chevron.right" size={9} weight="semibold" className={open ? 'chat-steps-chevron open' : 'chat-steps-chevron'} />
      </button>
      {open ? (
        <div className="chat-steps-list">
          {earlier > 0 ? <EarlierSteps count={earlier} open={openTrace} /> : null}
          {entries.slice(-VISIBLE_STEPS).map((e) => <ToolRowItem key={e.id} entry={e} />)}
        </div>
      ) : null}
    </div>
  )
}

/** What the host writes into a group when an outside bot joins, leaves, passes or doesn't answer. */
const ROOM_NOTICES = new Set(['botJoined', 'botLeft', 'botPassed', 'botTimedOut'])

/** A quiet room line; the bot's name in it carries its Bot capsule. */
function RoomNotice({ text, bot }: { text: string; bot?: { name?: string } }) {
  const name = bot?.name?.trim()
  const named = !!name && text.startsWith(name)
  return (
    <HStack gap={10} className="chat-notice-divider">
      <Hairline grow />
      <Text size="caption2" tone="tertiary" className="chat-notice-divider-text">
        {named ? <>{name}<BotTag />{text.slice(name.length)}</> : text}
      </Text>
      <Hairline grow />
    </HStack>
  )
}

/** Who wrote an outside bot's message and what to call it: its bot-guest chat, or a group it is in. */
function botPostOf(chat: Bot | null, entry: Entry, guestName: string | undefined): { name: string; provider: string } | null {
  const sender = entry.data.sender
  if (!sender?.botId || !chat || !(chat.botGuest || isGroup(chat))) return null
  return { name: guestName ?? (sender.name?.trim() || 'Bot'), provider: chat.botGuest?.provider ?? sender.provider ?? '' }
}

/** A reply that came after its run was over: an outlined bubble that quotes the message it answers, and why it starts nothing. */
function LateBubble({ entry, reason, quote, reply }: { entry: Entry; reason: string | null; quote: string | null; reply?: () => void }) {
  const store = useStore()
  const copy = () => entry.data.text && window.kith.app.copy(entry.data.text)
  return (
    <div className="message agent" title={fullDate.format(entry.createdAt)}>
      <div className="message-body">
        <Bubble
          className="bubble agent late selectable"
          reactions={reactionPick(store, entry)}
          items={() => [
            ...(reply ? [{ title: 'Reply', icon: 'arrowshape.turn.up.left', action: reply }] : []),
            { title: 'Copy', icon: 'square.on.square', action: copy },
          ]}
        >
          {quote ? (
            <div className="late-quote">
              <Icon name="arrowshape.turn.up.left" size={10} color="var(--secondary)" />
              <span>{quote}</span>
            </div>
          ) : null}
          <MarkdownText text={entry.data.text ?? ''} />
        </Bubble>
        {reason ? <Text as="div" size="footnote" tone="secondary" className="late-reason">{reason}</Text> : null}
      </div>
    </div>
  )
}
