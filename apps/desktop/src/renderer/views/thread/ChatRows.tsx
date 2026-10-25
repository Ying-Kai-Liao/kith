import { SharedFileView } from './SharedImage'
import { memo, useEffect, useRef, useState } from 'react'
import { isGroup, isWorking, needsInput, type Bot, type Entry, type ThreadSummary } from '@shared/models'
import { BotAvatar } from '../../components/Avatar'
import { Hairline, IconButton } from '../../components/Controls'
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
import { contextLabel } from './context-hint'
import { callLineSuffix } from './call-line'
import { BotGuestAvatar, ThreadChip as KithThreadChip, pairOf, type Origin, type Place } from '../../components/kith'
import { noticeThreadChip } from './notice-thread'
import { botGuestSender, botOriginLabel } from '@shared/bot-guest-chat'
import { runLabel, threadOrigin } from '@shared/thread-writer'
import { avatarImage } from '@shared/crew'

export function reactionPick(store: BotStore, entry: Entry): ReactionPick {
  return { emoji: QUICK_REACTIONS, chosen: entry.data.reactions ?? [], toggle: (e) => store.react(entry, e) }
}

/** One chat-visible entry in a chat or a thread, with what can be done to it. */
export const ChatRow = memo(function ChatRow({ entry, groupStart, chat, openTrace, openThread, replyInThread, openId = null }: {
  entry: Entry
  groupStart: boolean
  chat: Bot | null
  openTrace: () => void
  /** Main chat only: start (or open) the thread on this message. */
  openThread?: (e: Entry) => void
  /** "Reply in thread": opens the thread, never folds one that is open. */
  replyInThread?: (e: Entry) => void
  /** The thread open under its message: its chip tints in the agent's colours. */
  openId?: string | null
}) {
  const reply = openThread ? () => (replyInThread ?? openThread)(entry) : undefined
  const chip = (entry.kind === 'notice' ? noticeThreadChip(entry) : entry.data.thread && entry.data.thread.count > 0) && entry.data.thread && openThread ? <ThreadChip summary={entry.data.thread} open={() => openThread(entry)} isOpen={openId === entry.id} chat={chat} /> : null
  const guestSender = botGuestSender(chat, entry)
  switch (entry.kind) {
    case 'user':
      // The outside bot's own message in its read-only chat: the other party, on the left with its picture.
      if (guestSender && chat?.botGuest) {
        return (
          <VStack align="flex-start" gap={4} className={groupStart ? 'chat-row group-start' : 'chat-row'}>
            <HStack gap={8}>
              <BotGuestAvatar provider={chat.botGuest.provider} size={24} />
              <Text size="subheadline" weight="medium" tone="secondary">{guestSender.name}</Text>
            </HStack>
            <VStack gap={4} className="chat-row-indent">
              <GuestBotBubble entry={entry} reply={reply} />
              <ReactionsRow entry={entry} />
              {chip}
            </VStack>
          </VStack>
        )
      }
      return (
        <VStack align="flex-end" gap={4} className={groupStart ? 'chat-row group-start' : 'chat-row'}>
          <UserBubble entry={entry} botWorking={!!chat && isWorking(chat)} reply={reply} />
          <ReactionsRow entry={entry} />
          {chip}
        </VStack>
      )
    case 'agent':
      return (
        <VStack align="flex-start" gap={4} className={groupStart ? 'chat-row group-start' : 'chat-row'}>
          {chat && isGroup(chat) && groupStart ? <AuthorLabel botId={entry.data.author} /> : null}
          <AgentBubble entry={entry} openTrace={openTrace} reply={reply} />
          <VStack gap={4} className={chat && isGroup(chat) ? 'chat-row-indent' : undefined}>
            <AnsweredBy answeredBy={entry.data.answeredBy} />
            <ContextHint contextPercent={entry.data.contextPercent} />
            <ReactionsRow entry={entry} />
            {chip}
          </VStack>
        </VStack>
      )
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
      // Any notice that roots a thread (a bot guest's conversation) opens it like a message does.
      return <div className="chat-row notice">{chip && entry.data.thread?.origin === 'bot' ? null : <NoticeRow entry={entry} />}{chip ? <div className="notice-thread-chip">{chip}</div> : null}</div>
  }
})

/** A quiet "context N%" under the reply; the host sets it only above its threshold. */
function ContextHint({ contextPercent }: { contextPercent: unknown }) {
  const label = contextLabel(contextPercent)
  return label ? <Text size="footnote" tone="tertiary">{label}</Text> : null
}

/** Follows the store: the card shows a spinner while its answer is on its way. */
function LivePermission({ entry }: { entry: Entry }) {
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

/** Who wrote a message in a group: their avatar and name in their color. */
export function AuthorLabel({ botId }: { botId?: string | null }) {
  const store = useStore()
  const bot = botId ? store.bots.get(botId) : undefined
  return (
    <HStack gap={8}>
      {bot ? <BotAvatar bot={bot} size={26} animated={false} /> : null}
      <Text size="subheadline" weight="medium" tone="secondary">{store.authorName(botId)}</Text>
    </HStack>
  )
}

/**
 * Under a message with a thread: where it works (grey), then the pill with the agent's face, the reply count and
 * when. While the thread is open under its message the pill tints in the agent's colours.
 */
export function ThreadChip({ summary, open, isOpen = false, chat }: { summary: ThreadSummary; open: () => void; isOpen?: boolean; chat: Bot | null }) {
  const store = useStore()
  const author = summary.authors.map((id) => store.bots.get(id)).find((b): b is Bot => !!b) ?? (chat && !isGroup(chat) ? chat : null)
  const folder = chat?.folder
  const place: Place | undefined = folder && folder.kind !== 'workspace' && folder.name ? { kind: folder.kind === 'project' ? 'project' : 'folder', name: folder.name } : undefined
  const image = author ? avatarImage(author) : null
  // A thread started outside the app (a terminal, a channel) says so; an origin this app doesn't know shows nothing.
  const origin = threadOrigin(summary)
  const tag: Origin | undefined = origin === 'terminal' || origin === 'bot' || origin === 'discord' || origin === 'telegram' || origin === 'slack' ? origin : undefined
  return (
    <div className={`thread-meta ${chat && isGroup(chat) ? 'start' : ''}`}>
      <KithThreadChip
        thread={{
          replies: summary.count,
          newCount: summary.unread ?? 0,
          time: relativeTime.day(summary.lastAt),
          open: isOpen,
          // Without a face (a deleted bot) the chip still counts; the pill just has no picture.
          agent: { face: image ?? 'group', color: author?.avatarColor, id: author?.id ?? 'unknown' },
          pair: pairOf(author?.avatarColor),
          place,
          origin: tag,
          originLabel: origin === 'bot' ? botOriginLabel(summary) : runLabel(summary),
          onToggle: open,
          onOpen: open,
        }}
      />
    </div>
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
              { title: 'Copy', icon: 'square.on.square', action: copy },
              ...(reply ? [{ title: 'Reply in thread', icon: 'arrowshape.turn.up.left', action: reply }] : []),
              ...(status === 'queued' ? [{ title: 'Unsend', icon: 'arrow.uturn.backward', action: () => store.unsend(entry) }] : []),
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
          <Text size="caption2" tone="tertiary">{botWorking ? 'Queued until this response finishes' : 'Queued'}</Text>
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

/** An outside bot's message: an agent-style bubble on the left (copy and thread, no queue or unsend). */
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
            { title: 'Copy', icon: 'square.on.square', action: copy },
            ...(reply ? [{ title: 'Reply in thread', icon: 'arrowshape.turn.up.left', action: reply }] : []),
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
  const copy = () => entry.data.text && window.kith.app.copy(entry.data.text)
  return (
    <div className="message agent" title={fullDate.format(entry.createdAt)} onMouseEnter={() => setHovering(true)} onMouseLeave={() => setHovering(false)}>
      <div className="message-body">
        <Bubble
          className={entry.data.files?.length ? "file-message" : "bubble agent"}
          reactions={reactionPick(store, entry)}
          items={() => [
            { title: 'Copy', icon: 'square.on.square', action: copy },
            ...(reply ? [{ title: 'Reply in thread', icon: 'arrowshape.turn.up.left', action: reply }] : []),
            { title: 'Show what it did', icon: 'list.bullet', action: openTrace },
          ]}
        >
          {entry.data.files?.length ? <div className="file-message-cards">{entry.data.files.map((file) => <SharedFileView key={file.id} entry={entry} file={file} />)}</div> : <MarkdownText text={entry.data.text ?? ''} />}
        </Bubble>
        <MessageActions className="trailing" visible={hovering} reactions={reactionPick(store, entry)} reply={reply} trace={openTrace} copy={copy} />
      </div>
    </div>
  )
}

export function NoticeRow({ entry }: { entry: Entry }) {
  const text = entry.data.text ?? ''
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
  const reactions = entry.data.reactions
  if (!reactions?.length) return null
  return (
    <HStack align="stretch" gap={4}>
      {reactions.map((emoji) => (
        <button key={emoji} className="press reaction-chip" title={`Remove ${emoji}`} aria-label={`Remove reaction ${emoji}`} onClick={() => store.react(entry, emoji)}>
          <Text size="body">{emoji}</Text>
        </button>
      ))}
    </HStack>
  )
}

/** Compact actions beside the bubble: a reaction strip, reply in thread, and more (copy, what it did). */
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
      {reply ? <IconButton title="Reply in thread" icon="arrowshape.turn.up.left" size={26} onClick={reply} /> : null}
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
