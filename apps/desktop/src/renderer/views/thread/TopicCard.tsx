import { useLayoutEffect, useState, type ReactNode } from 'react'
import { isWorking, type Entry } from '@shared/models'
import { avatarImage } from '@shared/crew'
import { BotAvatar, SkyPatch, pairOf } from '../../components/kith'
import { pairStyle } from '../../components/kith/pair'
import { Icon } from '../../components/Icon'
import { AnchoredMenu, useContextMenu } from '../../components/Overlay'
import { ThinkingOrb } from '../../components/ThinkingOrb'
import { useStore as useLiveStore } from '../../store/context'
import type { BotStore } from '../../store/bot-store'
import { OpenCard } from './ChatRows'
import { relativeTime } from './chat-items'
import type { ChatItem } from './flat-chat'
import { askRenameTopic } from './TopicPrompt'
import './topic.css'

type TopicItem = Extract<ChatItem, { kind: 'topic' }>

/** Who wrote a user or agent message in a topic: a key for counting faces, and the name its latest line starts with. */
function authorOf(store: BotStore, e: Entry) {
  if (e.kind === 'agent') {
    const id = e.data.author ?? e.botId
    return { key: `agent:${id}`, name: store.authorName(id), bot: id }
  }
  const sender = e.data.sender
  return sender?.botId ? { key: `bot:${sender.botId}`, name: sender.name ?? 'Bot', bot: null } : { key: 'you', name: 'You', bot: null }
}

/** One face of a topic card: an agent's bot face, or a person's sky patch. */
function Face({ store, who }: { store: BotStore; who: ReturnType<typeof authorOf> }) {
  const bot = who.bot ? store.bots.get(who.bot) : undefined
  return bot
    ? <BotAvatar face={avatarImage(bot) ?? 'group'} color={bot.avatarColor} id={bot.id} size={24} animated={false} />
    : <SkyPatch id={who.key} you={who.key === 'you'} size={24} />
}

const firstLine = (text: string | undefined) => (text ?? '').trim().split('\n')[0]!.trim()

/**
 * A topic the owner split off: the run folds into this one card (the two faces, its title, the newest line, how many
 * messages, the time). A question inside that waits on the owner rises onto the card. Tap opens the rows as normal
 * chat rows, in place; the room goes on below. No context ring: a topic isn't a session.
 */
export function TopicCard({ item, renderRow, focusId }: { item: TopicItem; renderRow: (item: ChatItem) => ReactNode; focusId?: string | null }) {
  const store = useLiveStore()
  const [open, setOpen] = useState(false)
  const menu = useContextMenu()
  const { root, topic, entries } = item
  // A jump to a message inside the topic (search, a notification) opens the card so the row itself can be reached.
  const holdsFocus = !!focusId && entries.some((e) => e.id === focusId)
  useLayoutEffect(() => {
    if (holdsFocus) setOpen(true)
  }, [holdsFocus, focusId])
  const said = entries.filter((e) => e.kind === 'user' || e.kind === 'agent')
  const faces = new Map<string, ReturnType<typeof authorOf>>()
  for (const e of said) {
    const who = authorOf(store, e)
    if (faces.size < 2 && !faces.has(who.key)) faces.set(who.key, who)
  }
  const latest = said.filter((e) => firstLine(e.data.text)).at(-1)
  const newest = entries.reduce((m, e) => Math.max(m, e.createdAt), 0)
  const waiting = entries.filter((e) => e.kind === 'permission' && e.data.status === 'pending')
  // A turn still running in this chat, whose newest row is in the topic: the card shows it at work.
  const bot = store.bots.get(root.botId)
  const chatEnd = store.allEntries(root.botId).at(-1)
  const working = !!bot && isWorking(bot) && !!chatEnd && entries.some((e) => e.id === chatEnd.id)
  const owner = store.isOwner
  // Waiting on you: the card takes the colour pair of the agent that asked, at low strength.
  const tint = waiting.length ? pairStyle(pairOf(store.bots.get(waiting[0]!.data.author ?? waiting[0]!.botId)?.avatarColor)) : undefined
  return (
    <div className="chat-row notice">
      <div className={waiting.length ? 'topic-card waiting' : 'topic-card'} style={tint} onContextMenu={owner ? menu.onContextMenu : undefined}>
        <button className="topic-head press" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
          <span className="topic-faces">{[...faces.values()].map((who) => <Face key={who.key} store={store} who={who} />)}</span>
          <span className="topic-main">
            <span className="topic-title-row">
              <span className="topic-title">{topic.title}</span>
              <span className="topic-time">{relativeTime.time(newest)}</span>
            </span>
            {latest ? <span className="topic-line">{`${authorOf(store, latest).name}: ${firstLine(latest.data.text)}`}</span> : null}
            <span className="topic-meta">
              {said.length === 1 ? '1 message' : `${said.length} messages`}
              {working ? <ThinkingOrb size={12} color="var(--secondary)" /> : null}
            </span>
          </span>
          <span className={`disclosure-chevron${open ? ' open' : ''}`}><Icon name="chevron.right" size={10} weight="semibold" /></span>
        </button>
        {waiting.map((e) => <div key={e.id} className="topic-needs"><OpenCard entry={e} /></div>)}
        {open ? (
          <div className="topic-rows">
            {item.inner.map((row) => <div key={row.id} data-item={row.id}>{renderRow(row)}</div>)}
          </div>
        ) : null}
      </div>
      <AnchoredMenu
        open={menu.open}
        onClose={menu.close}
        point={menu.point}
        items={() => [
          { title: 'Rename', icon: 'pencil', action: () => askRenameTopic(root) },
          { title: 'Unfold', icon: 'arrow.up.left.and.arrow.down.right', action: () => { void store.unfoldTopic(root).catch(() => {}) } },
        ]}
      />
    </div>
  )
}
