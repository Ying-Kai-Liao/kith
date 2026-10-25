import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { isGroup, isWorkingIn, type Bot, type Entry } from '@shared/models'
import { threadFeed, toolRowOf } from '@shared/thread-feed'
import { IconButton } from '../../components/Controls'
import { pairOf, ToolRow } from '../../components/kith'
import { pairStyle } from '../../components/kith/pair.ts'
import { AnchoredMenu, Sheet } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { PlaceFolderControl } from '../bots/PlaceFolderPicker'
import { useArrivals } from './chat-items'
import { ChatRow, WorkingIndicator } from './ChatRows'
import { TraceView } from './TraceView'
import { useReading } from './reading'

/**
 * An open thread, hanging under its message: a thin line in the agent's colours, the replies, tool rows (the newest
 * four of a run) and the bot at work. In a bot's own chat the thread is a separate branch (its own session); in a
 * group the room answers inside it.
 */
export function InlineThread({ botId, rootId, close }: { botId: string; rootId: string; close: () => void }) {
  const store = useStore()
  const chat = store.bots.get(botId) ?? null
  const root = store.allEntries(botId).find((e) => e.id === rootId)
  const replies = store.replies(botId, rootId)
  const feed = threadFeed(replies)
  const live = !!chat && isWorkingIn(chat, botId, rootId) && !store.isOffline
  const arrivals = useArrivals([...feed.map((f) => f.id), ...(live ? ['working'] : [])], true)
  const [showTrace, setShowTrace] = useState(false)
  const [menu, setMenu] = useState(false)
  const more = useRef<HTMLButtonElement>(null)
  const self = useRef<HTMLDivElement>(null)
  useReading(store, botId, rootId)

  useEffect(() => {
    void store.loadThread(botId, rootId)
  }, [store, botId, rootId])

  // Opening brings the thread's start into view without moving what the reader was looking at.
  useLayoutEffect(() => {
    self.current?.scrollIntoView({ block: 'nearest' })
  }, [])

  // The thread's agent: who replied first, else the chat's own bot.
  const author = (root?.data.thread?.authors ?? []).map((id) => store.bots.get(id)).find((b): b is Bot => !!b) ?? (chat && !isGroup(chat) ? chat : null)
  const openTrace = () => setShowTrace(true)
  const count = root?.data.thread?.count ?? replies.length
  return (
    <div className="inline-thread" ref={self} style={pairStyle(pairOf(author?.avatarColor))}>
      <div className="inline-thread-head">
        <Text size="caption" tone="tertiary" className="inline-thread-label">{count === 0 ? 'No replies yet' : count === 1 ? '1 reply' : `${count} replies`}</Text>
        {chat && !isGroup(chat) && !chat.botGuest ? <PlaceFolderControl botId={botId} threadId={rootId} folder={store.threadFolder(botId, rootId)} messages={replies.length} /> : null}
        <span className="inline-thread-spacer" />
        <button ref={more} className="inline-thread-more" title="Thread actions" aria-label="Thread actions" onClick={() => setMenu(true)}>
          <span aria-hidden>•••</span>
        </button>
        <IconButton title="Fold thread" icon="chevron.up" size={24} onClick={close} />
        <AnchoredMenu
          open={menu}
          onClose={() => setMenu(false)}
          anchor={more}
          items={() => [
            { title: 'Show what it did', icon: 'list.bullet', action: openTrace },
            { title: 'Fold thread', icon: 'chevron.up', action: close },
          ]}
        />
      </div>
      <div className="inline-thread-body">
        {feed.map((f) =>
          f.kind === 'entry' ? (
            <div key={f.id} className={arrivals.has(f.id) ? (f.entry.kind === 'user' ? 'row-in mine' : 'row-in') : undefined}>
              <ChatRow entry={f.entry} groupStart={f.groupStart} chat={chat} openTrace={openTrace} />
            </div>
          ) : f.kind === 'tool' ? (
            <InlineTool key={f.id} entry={f.entry} />
          ) : (
            <button key={f.id} className="inline-thread-earlier" onClick={openTrace}>
              {f.count === 1 ? '1 earlier step' : `${f.count} earlier steps`}
            </button>
          ),
        )}
        {chat && live ? (
          <div className={arrivals.has('working') ? 'chat-working row-in' : 'chat-working'}>
            <WorkingIndicator bot={chat} thinking={store.currentThinking(botId, rootId)} />
          </div>
        ) : null}
      </div>
      <Sheet open={showTrace} onClose={() => setShowTrace(false)} width={620} height={560}>
        <TraceView botId={botId} thread={rootId} />
      </Sheet>
    </div>
  )
}

function InlineTool({ entry }: { entry: Entry }) {
  const m = toolRowOf(entry)
  return <ToolRow verb={m.verb} object={m.object} result={m.result} state={m.state} detail={m.command || m.output ? { command: m.command, output: m.output } : undefined} />
}
