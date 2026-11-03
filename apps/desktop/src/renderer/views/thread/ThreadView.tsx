import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { botGuestReadOnlyLine } from '@shared/bot-guest-chat'
import { draftOf, isGroup, isWorkingIn, type Bot, type BotDraft } from '@shared/models'
import { isTask, taskTitle, workingThreadIn } from '@shared/tasks'
import { BotAvatar, GroupAvatar } from '../../components/Avatar'
import { DropdownMenu, Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { LiveDot, pairOf, StatusChip } from '../../components/kith'
import { SHOW_COMPUTER_EVENT } from '../../lib/computer-event'
import type { BotStore } from '../../store/bot-store'
import { chatSubtitle, showsComputerItem } from './chat-subtitle'
import { Sheet, usePresence } from '../../components/Overlay'
import { HStack, Spacer } from '../../components/Stack'
import { Text } from '../../components/Text'
import { prefs, usePref } from '../../lib/prefs'
import { useStore } from '../../store/context'
import { UpdateNeededCard } from '../UpdateNeededCard'
import { buildChat, rowIn, useArrivals, type ChatItem } from './chat-items'
import { BotConversationView, BotMessageRow } from './BotConversation'
import { TopicCard } from './TopicCard'
import { TaskCard } from './TaskCard'
import { TopicPromptHost } from './TopicPrompt'
import { ChatRow, lateQuoteOf, StepsRow, TimeSeparator, WorkingIndicator } from './ChatRows'
import { AddToRoomSheet } from './AddToRoomSheet'
import { Composer } from './Composer'
import { EndLine, LiveLine } from './ConversationLine'
import { isOpen } from '@shared/conversation'
import { botGuestsFor } from '../../store/bot-guests-store'
import { ChatSidePanel } from './ChatSidePanel'
import { DetailsPanel, type DetailsTab } from './DetailsPanel'
import { TraceView } from './TraceView'
import { useReading } from './reading'
import { PlaceFolderControl } from '../bots/PlaceFolderPicker'
import { GroupEditorView } from '../bots/GroupEditorView'
import { BotTemplateView } from '../bots/BotTemplateView'
import { CallView } from '../call/CallView'
import { START_CALL_EVENT } from '../../lib/call-event'
import { callBlockedReason } from '../call/call-availability'
import { MacChatButton } from './MacChatButton'
import { ShareAgentSheet, useCanShareAgent } from '../ShareAgentSheet'
import './thread.css'
import './composer.css'
import './chat-controls.css'

const WIDE = 680

/**
 * One endless, flat conversation with a bot or a group chat: messages, the replies of every task and thread in time
 * order, and tool calls as rows. Thinking lives in the "Full conversation" sheet.
 */
export const ThreadView = memo(function ThreadView({ botId }: { botId: string }) {
  const store = useStore()
  const bot = store.bots.get(botId) ?? null
  const [showTrace, setShowTrace] = useState(false)
  // A conversation opened from elsewhere (the bot detail, a notification): the chat scrolls to its first message.
  const [focusId, setFocusId] = useState<string | null>(null)
  const [botChat, setBotChat] = useState<string | null>(null)
  const [editingGroup, setEditingGroup] = useState(false)
  const [addingMember, setAddingMember] = useState(false)
  const [sharing, setSharing] = useState(false)
  const canShare = useCanShareAgent(store, bot)
  const [templateDraft, setTemplateDraft] = useState<BotDraft | null>(null)
  const [showSettings, setShowSettings] = useState(false)
  const [detailsTab, setDetailsTab] = useState<DetailsTab>('details')
  const [compactDetails, setCompactDetails] = useState(false)
  const [calling, setCalling] = useState(false)
  const [callSpeaking, setCallSpeaking] = useState(false)
  const interruptCall = useRef<(() => void) | null>(null)
  const guestChat = bot?.botGuest ?? null
  // A group's room run: the live line above the box while one is open in this chat's main lane (nothing in a 1:1 chat).
  const group = !!bot && isGroup(bot)
  const run = group ? store.conversations.inLane(botId, null) : null
  const liveRun = run && isOpen(run) && !store.isOffline ? run : null
  // What the run did while this window was away comes from the host, and the owner's bots list names who is in the room.
  const online = store.connection.kind === 'online'
  useEffect(() => {
    if (!group || !online) return
    void store.refreshConversation(botId)
    void botGuestsFor(store).load()
  }, [store, botId, group, online])
  // A bot guest's chat is read-only here (v1): no calls, no composer, no place picker.
  const callBlocked = guestChat ? 'Read only' : callBlockedReason(store)
  // One call per chat window: a second press (double click, or the details page while in a call) does nothing.
  const startCall = () => {
    if (calling || callBlocked) return
    setCompactDetails(false)
    setCalling(true)
  }
  // A call the person just accepted opens here and starts; the agent then speaks first.
  const [greet, setGreet] = useState<string | null>(null)
  useEffect(() => {
    const id = store.takeCallStart(botId)
    if (!id) return
    setGreet(id)
    setCompactDetails(false)
    setCalling(true)
  })
  useEffect(() => {
    window.addEventListener(START_CALL_EVENT, startCall)
    return () => window.removeEventListener(START_CALL_EVENT, startCall)
  })
  const [routineId, setRoutineId] = useState<string | null>(null)
  const [routineRequest, setRoutineRequest] = useState(0)
  const root = useRef<HTMLDivElement>(null)
  const dock = useRef<HTMLDivElement>(null)
  // The width is measured, not rendered: resizing re-renders only when `wide` flips, or while the
  // window is narrow enough to squeeze the open side panel.
  const width = useRef(800)
  const [wide, setWide] = useState(true)
  const [, setSqueezed] = useState(0)
  const squeezeBelow = useRef(0)
  const [detailsWidth, setDetailsWidth] = usePref(prefs.detailsPanelWidth)
  useReading(store, botId, null)

  useLayoutEffect(() => {
    const el = root.current
    if (!el) return
    const observer = new ResizeObserver(([e]) => {
      const w = e!.contentRect.width
      width.current = w
      setWide(w >= WIDE)
      setSqueezed(w - 380 < squeezeBelow.current ? w : 0)
    })
    observer.observe(el)
    // The composer floats over the transcript; its height becomes the transcript's bottom padding.
    const docked = new ResizeObserver(([e]) => el.style.setProperty('--composer-height', `${Math.ceil(e!.borderBoxSize[0]!.blockSize)}px`))
    if (dock.current) docked.observe(dock.current)
    return () => {
      observer.disconnect()
      docked.disconnect()
    }
  }, [])

  const panelOpen = wide && showSettings
  const preferredPanel = detailsWidth
  squeezeBelow.current = panelOpen ? preferredPanel : 0
  // The side panel's edge drags its width, leaving the conversation at least 380 wide.
  const panelWidth = (w: number) => Math.min(Math.max(260, w), Math.min(420, width.current - 380))
  // A compact Details presentation dismisses once the window is wide enough for the inspector.
  useEffect(() => {
    if (wide) setCompactDetails(false)
  }, [wide])
  const toggleDetails = useCallback(() => {
    if (!wide) setCompactDetails((c) => !c)
    else setShowSettings((s) => !s)
  }, [wide])

  useEffect(
    () =>
      window.kith.app.onCommand((c) => {
        if (c.kind === 'toggleDetails') toggleDetails()
      }),
    [toggleDetails],
  )

  const openTrace = useCallback(() => setShowTrace(true), [])
  // The bot detail opens a conversation: the chat, at that message.
  useEffect(() => {
    const want = store.pendingThread
    if (want && want.botId === botId) {
      store.pendingThread = null
      setFocusId(want.root)
    }
  }, [store, botId, store.pendingThread])

  const presentRoutine = useCallback((id: string | null) => {
    setRoutineId(id)
    setRoutineRequest((r) => r + 1)
    setDetailsTab('details')
    setShowSettings(true)
    if (!wide) setCompactDetails(true)
  }, [wide])

  const details = (
    <DetailsPanel
      key={routineRequest}
      botId={botId}
      routineId={routineId}
      callBlocked={callBlocked}
      onCall={bot && !calling ? startCall : null}
      tab={detailsTab}
      setTab={setDetailsTab}
      editGroup={() => setEditingGroup(true)}
      addMember={() => setAddingMember(true)}
      share={() => {
        setCompactDetails(false)
        if (bot) setTemplateDraft(draftOf(bot))
      }}
      close={() => {
        setShowSettings(false)
        setCompactDetails(false)
      }}
    />
  )

  const computerItem = showsComputerItem(computerFacts(store, botId))

  return (
    <div className="thread-view" ref={root}>
      <div className="conversation">
        <div className="top-chrome">
          <TopFade />
          <div className="top-chrome-row">
            <Spacer />
            {/* One glass pair: call | more (the details toggle, also on the pill and with the details shortcut). */}
            <div className="top-pair kith-glass">
              {bot ? (
                <button className="top-pair-button" title={callBlocked ?? 'Voice call'} aria-label={callBlocked ?? 'Voice call'} disabled={!!callBlocked || calling} onClick={startCall}>
                  <Icon name="phone.fill" size={15} weight="medium" />
                </button>
              ) : null}
              {(!showSettings || !wide) && !compactDetails && (canShare || computerItem) ? (
                <DropdownMenu
                  className="top-pair-button"
                  title="More"
                  items={() => [
                    { title: 'Conversation details', icon: 'info.circle', action: toggleDetails },
                    ...(canShare ? [{ title: 'Share access…', icon: 'person.crop.circle.badge.plus', action: () => setSharing(true) }] : []),
                    ...(computerItem ? [{ title: 'Show the computer', icon: 'display', action: showComputer }] : []),
                  ]}
                >
                  <Icon name="ellipsis" size={15} weight="medium" />
                </DropdownMenu>
              ) : (!showSettings || !wide) && !compactDetails ? (
                <button className="top-pair-button" title="Conversation details" aria-label="Conversation details" onClick={toggleDetails}>
                  <Icon name="ellipsis" size={15} weight="medium" />
                </button>
              ) : null}
            </div>
          </div>
          {/* The call bar takes the pill's place; its frosted glass would show the name blurred. */}
          <div className="title-stack">
          <button className={`title-pill kith-glass ${calling ? 'hidden-by-call' : ''}`} inert={calling} onClick={toggleDetails} aria-label={`${bot?.name ?? 'Conversation'}, view details`} title="View conversation details">
            <HStack as="span" gap={7}>
              {bot ? isGroup(bot) ? <GroupAvatar members={store.members(bot)} size={26} /> : <BotAvatar bot={bot} size={26} /> : null}
              <span className="title-pill-name">{bot?.name ?? ''}</span>
              <span className="title-pill-chevron">
                <Icon name="chevron.down" size={10} weight="semibold" color="var(--secondary)" />
              </span>
            </HStack>
            <ChatSubtitle botId={botId} />
          </button>
          {bot && !isGroup(bot) && !guestChat ? (
            <div className={`place-chip-row ${calling ? 'hidden-by-call' : ''}`} inert={calling}>
              <PlaceFolderControl botId={botId} folder={bot.folder} messages={store.chat(botId).length} />
            </div>
          ) : null}
          </div>
        </div>
        <Transcript botId={botId} focusId={focusId} openTrace={openTrace} openRoutine={presentRoutine} openBotChat={setBotChat} />
        <div className="composer-dock" ref={dock}>
          <div className="composer-fade" />
          {bot && guestChat ? (
            <Text as="div" size="footnote" tone="secondary" className="composer-read-only">{botGuestReadOnlyLine(guestChat, bot.name)}</Text>
          ) : store.mismatch ? (
            <div className="composer-update-needed">
              <UpdateNeededCard />
            </div>
          ) : (
            <>
            {liveRun ? <LiveLine conversation={liveRun} botId={botId} lane={null} /> : <WorkingNow botId={botId} />}
            <Composer
              botId={botId}
              onCall={!calling && bot && !callBlocked ? startCall : null}
              onInterrupt={callSpeaking ? () => interruptCall.current?.() : null}
              liveLine={!!liveRun}
            />
            </>
          )}
        </div>
        {calling ? (
          <CallView botId={botId} greet={greet} onSpeaking={setCallSpeaking} interrupt={interruptCall} onEnd={() => { setCalling(false); setGreet(null) }} />
        ) : null}
      </div>
      <ChatSidePanel
        open={panelOpen}
        width={panelWidth(preferredPanel)}
        min={260}
        max={Math.min(420, width.current - 380)}
        onResize={(w) => setDetailsWidth(panelWidth(w))}
      >
        {details}
      </ChatSidePanel>

      <Sheet open={compactDetails} onClose={() => setCompactDetails(false)} width={380} height={600}>
        {details}
      </Sheet>
      <Sheet open={botChat !== null} onClose={() => setBotChat(null)} width={620} height={560}>
        {botChat ? <BotConversationView botId={botId} peerId={botChat} /> : null}
      </Sheet>
      <Sheet open={showTrace} onClose={() => setShowTrace(false)} width={620} height={560}>
        <TraceView botId={botId} />
      </Sheet>
      <Sheet open={templateDraft !== null} onClose={() => setTemplateDraft(null)}>
        {templateDraft ? <BotTemplateView draft={templateDraft} /> : null}
      </Sheet>
      {bot && canShare ? <ShareAgentSheet store={store} bot={bot} open={sharing} onClose={() => setSharing(false)} /> : null}
      <Sheet open={addingMember} onClose={() => setAddingMember(false)} width={420} height={620}>
        {bot && isGroup(bot) ? <AddToRoomSheet group={bot} /> : null}
      </Sheet>
      <Sheet open={editingGroup} onClose={() => setEditingGroup(false)} width={420} height={560}>
        <GroupEditorView group={bot} />
      </Sheet>
    </div>
  )
})

/** The top edge of the chat: messages fade out gradually as they scroll under the title pill. */
function TopFade() {
  return <div className="top-fade" />
}

/** Under the name: a status chip when the connection isn't Live, "using the computer" while the agent drives the screen. */
function ChatSubtitle({ botId }: { botId: string }) {
  const store = useStore()
  const facts = computerFacts(store, botId)
  const subtitle = chatSubtitle(facts, store.shownConnection, !!store.mismatch, store.hostName)
  if (!subtitle) return null
  if (subtitle.kind === 'chip') {
    return (
      <span className="title-pill-subtitle" title={`${store.hostName} · ${store.connectionLabel}`}>
        <StatusChip kind={subtitle.chip} detail={subtitle.detail} />
      </span>
    )
  }
  return (
    <span
      role="button"
      tabIndex={0}
      className="title-pill-subtitle title-pill-using"
      title="Show the computer"
      onClick={(e) => { e.stopPropagation(); showComputer() }}
      onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); showComputer() } }}
    >
      <HStack as="span" gap={5}>
        <Icon name="display" size={10} weight="medium" />
        <Text size="caption" weight="medium" truncate>using the computer</Text>
      </HStack>
    </span>
  )
}

function computerFacts(store: BotStore, botId: string) {
  return { bot: store.bots.get(botId), isLoopback: store.isLoopback, isOwner: store.isOwner, screen: store.screen }
}

const showComputer = () => window.dispatchEvent(new Event(SHOW_COMPUTER_EVENT))

/** The messages: every loaded message, brought to the newest as it changes while the reader is at the bottom. */
const PAGE = 40

const Transcript = memo(function Transcript({ botId, focusId, openTrace, openRoutine, openBotChat }: { botId: string; focusId: string | null; openTrace: () => void; openRoutine: (id: string | null) => void; openBotChat: (peerId: string) => void }) {
  const store = useStore()
  const bot = store.bots.get(botId) ?? null
  // The turn at work, wherever it runs (the chat, a task, a thread): one indicator at the end.
  const working = bot ? workingThreadIn(bot, botId) : undefined
  // The room's live line above the box says who is up, so the working indicator would say it twice.
  const run = bot && isGroup(bot) ? store.conversations.inLane(botId, null) : null
  const live = !!bot && working !== undefined && !store.isOffline && !(run && isOpen(run))
  // Rebuilt only when this chat changes: the store replaces a chat's list on every change.
  const entries = store.allEntries(botId)
  const endVersion = store.conversations.version
  const all = useMemo(() => buildChat(entries, store.conversations.endedIn(botId)), [entries, botId, endVersion]) // eslint-disable-line react-hooks/exhaustive-deps
  // The newest 40 items first; older pages come in near the top.
  const [firstShown, setFirstShown] = useState<string | null>(null)
  const [loadingEarlier, setLoadingEarlier] = useState(false)
  const found = firstShown ? all.findIndex((i) => i.id === firstShown) : -1
  const start = found >= 0 ? found : Math.max(0, all.length - PAGE)
  const items = all.slice(start)
  const more = start > 0 || store.canLoadOlder(botId)
  // Following the newest message, or reading history: scrolling up releases the bottom,
  // arriving back at the end (or Jump to latest) restores it. Content growth alone never changes it.
  const [following, setFollowing] = useState(true)
  const followingRef = useRef(true)
  followingRef.current = following
  const scroller = useRef<HTMLDivElement>(null)
  const lastScrollTop = useRef(0)
  const keepPlace = useRef<number | null>(null)
  const last = items[items.length - 1]
  const jump = usePresence(!following && items.length > 0, 200)
  // The replies of threads on screen are read here, in the chat.
  const rootOf = (i: ChatItem) => (i.kind === 'entry' ? i.entry : i.kind === 'task' ? i.root : null)
  const unread = items.flatMap((i) => ((rootOf(i)?.data.thread?.unread ?? 0) > 0 ? [rootOf(i)!.id] : []))
  // Work in a task shows on its card's live line, so the chat's own indicator is for work outside the cards.
  const inCard = !!working && all.some((i) => i.kind === 'task' && i.root.id === working)
  const arrivals = useArrivals([...items.map((i) => i.id), ...(live && !inCard ? ['working'] : [])], following)

  const toBottom = (smooth = false) => {
    const el = scroller.current
    if (el) el.scrollTo({ top: el.scrollHeight, behavior: smooth ? 'smooth' : 'auto' })
  }

  useEffect(() => {
    if (!firstShown && items[0]) setFirstShown(items[0].id)
  }, [firstShown, items])

  const showEarlier = async () => {
    if (loadingEarlier) return
    setLoadingEarlier(true)
    const el = scroller.current
    keepPlace.current = el ? el.scrollHeight - el.scrollTop : null
    let list = all
    let at = start
    if (at === 0) {
      const anchor = list[0]?.id
      await store.loadOlder(botId)
      list = buildChat(store.allEntries(botId), store.conversations.endedIn(botId))
      at = anchor ? Math.max(0, list.findIndex((i) => i.id === anchor)) : 0
    }
    if (at > 0) setFirstShown(list[Math.max(0, at - PAGE)]!.id)
    setLoadingEarlier(false)
  }

  // Every thread's replies are part of this chat: fetch the ones of the messages on screen that aren't here yet.
  const asked = useRef(new Set<string>())
  useEffect(() => {
    for (const item of items) {
      const root = rootOf(item)
      if (!root) continue
      const count = root.data.thread?.count ?? 0
      const key = `${root.id}:${count}`
      if (count === 0 || asked.current.has(key) || store.replies(botId, root.id).length >= count) continue
      asked.current.add(key)
      void store.loadThread(botId, root.id)
    }
  })

  // A conversation opened from the bot detail or a notification: its first message into view, the chat left there.
  useLayoutEffect(() => {
    if (!focusId) return
    const at = all.findIndex((i) => (i.kind === 'entry' && i.entry.id === focusId) || (i.kind === 'topic' && i.entries.some((e) => e.id === focusId)) || (i.kind === 'task' && (i.root.id === focusId || i.thread.some((e) => e.id === focusId))))
    if (at < 0) {
      void store.loadThread(botId, focusId)
      return
    }
    setFollowing(false)
    followingRef.current = false
    if (at < start) setFirstShown(all[Math.max(0, at - 3)]!.id)
    // Inside a topic the card has opened by now: the row itself, else the card.
    requestAnimationFrame(() => {
      const find = (id: string) => scroller.current?.querySelector(`[data-item="${CSS.escape(id)}"]`)
      ;(find(focusId) ?? find(all[at]!.id))?.scrollIntoView({ block: 'start' })
    })
  }, [focusId, all])

  // Earlier messages going in above keep the reader's place; a follower stays at the bottom.
  useLayoutEffect(() => {
    const el = scroller.current
    if (!el) return
    if (keepPlace.current !== null && !followingRef.current) {
      el.scrollTop = el.scrollHeight - keepPlace.current
      keepPlace.current = null
    } else if (followingRef.current) toBottom()
  })

  // A message the user sends brings the chat back to the newest.
  const newest = last?.kind === 'entry' ? last.entry : last?.kind === 'task' ? (last.thread.at(-1) ?? last.root) : null
  const lastIsUser = newest?.kind === 'user'
  useEffect(() => {
    if (lastIsUser) setFollowing(true)
  }, [newest?.id, lastIsUser])

  // Rows that grow after layout (images, the text size) keep a follower at the bottom.
  useEffect(() => {
    const el = scroller.current
    if (!el) return
    const observer = new ResizeObserver(() => {
      if (followingRef.current) toBottom()
    })
    observer.observe(el.firstElementChild!)
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  return (
    <div className="transcript-frame">
      <div
        className="transcript"
        ref={scroller}
        role="log"
        aria-label="Messages"
        onScroll={(e) => {
          const el = e.currentTarget
          const atEnd = el.scrollHeight - el.scrollTop - el.clientHeight < 8
          if (el.scrollTop < lastScrollTop.current - 1 && !atEnd) setFollowing(false)
          else if (atEnd) setFollowing(true)
          lastScrollTop.current = el.scrollTop
          if (more && el.scrollTop < 200 && !loadingEarlier) void showEarlier()
        }}
      >
        <div className="transcript-content">
          {more ? (
            <button className="load-earlier" disabled={loadingEarlier} onClick={() => void showEarlier()}>
              {loadingEarlier ? <Spinner size={14} /> : 'Load earlier messages'}
            </button>
          ) : null}
          {items.length === 0 && bot ? <div className="transcript-intro">{isGroup(bot) ? <GroupIntroCard group={bot} /> : <IntroCard bot={bot} />}</div> : null}
          {items.map((item) => (
            <div key={item.id} data-item={item.id} className={arrivals.has(item.id) ? rowIn(item) : undefined}>
              <Row item={item} chat={bot} focusId={focusId} openTrace={openTrace} openRoutine={openRoutine} openBotChat={openBotChat} />
            </div>
          ))}
          {unread.map((id) => <ReadingThread key={id} botId={botId} root={id} />)}
          {bot && live && !inCard ? (
            <div className={arrivals.has('working') ? 'chat-working row-in' : 'chat-working'}>
              <WorkingIndicator bot={bot} thinking={store.currentThinking(botId, working ?? null)} />
            </div>
          ) : null}
          <div className="chat-end" />
          <TopicPromptHost />
        </div>
      </div>
      {jump.mounted ? (
        <div className={`jump-to-latest ${jump.shown ? 'shown' : ''}`} inert={!jump.shown}>
          <MacChatButton
            title="Jump to latest"
            icon="arrow.down"
            direction={[0, 1]}
            onClick={() => {
              setFollowing(true)
              toBottom(true)
            }}
          />
        </div>
      ) : null}
    </div>
  )
})

/**
 * A task at work whose card is off screen: one line above the box says so ("● Board badge fix  Running tests · 1:12"),
 * with how many more are at work; a click brings the card into view.
 */
function WorkingNow({ botId }: { botId: string }) {
  const store = useStore()
  const bot = store.bots.get(botId)
  const roots = bot && bot.tasks ? store.chat(botId).filter((e) => isTask(e) && isWorkingIn(bot, botId, e.id)) : []
  const key = roots.map((e) => e.id).join(' ')
  const [hidden, setHidden] = useState<Set<string>>(new Set())
  useEffect(() => {
    if (!key) return
    const ids = key.split(' ')
    const seen = new Map<string, boolean>()
    const update = () => setHidden(new Set(ids.filter((id) => !seen.get(id))))
    const observer = new IntersectionObserver((records) => {
      for (const r of records) seen.set((r.target as HTMLElement).dataset.task!, r.isIntersecting)
      update()
    }, { root: document.querySelector('.transcript') })
    const watch = () => {
      observer.disconnect()
      for (const id of ids) {
        const el = document.querySelector(`.transcript [data-task="${CSS.escape(id)}"]`)
        if (el) observer.observe(el)
        else seen.set(id, false)
      }
      update()
    }
    watch()
    // Cards mount and unmount as the chat pages; look again when the list changes.
    const list = document.querySelector('.transcript-content')
    const mutations = new MutationObserver(watch)
    if (list) mutations.observe(list, { childList: true })
    return () => {
      observer.disconnect()
      mutations.disconnect()
    }
  }, [key])
  const off = roots.filter((e) => hidden.has(e.id))
  const first = off.at(-1)
  if (!bot || !first) return null
  return (
    <button className="working-now press" onClick={() => document.querySelector(`.transcript [data-task="${CSS.escape(first.id)}"]`)?.scrollIntoView({ block: 'center', behavior: 'smooth' })}>
      <LiveDot pair={pairOf(bot.avatarColor)} />
      <b>{taskTitle(first)}</b>
      <span className="working-now-what">{bot.activity || 'Working…'}</span>
      {off.length > 1 ? <span className="working-now-more">{`+${off.length - 1}`}</span> : null}
    </button>
  )
}

/** A thread's replies are on screen in the chat: they are read. */
function ReadingThread({ botId, root }: { botId: string; root: string }) {
  useReading(useStore(), botId, root)
  return null
}

function Row({ item, chat, focusId, openTrace, openRoutine, openBotChat }: { item: ChatItem; chat: Bot | null; focusId?: string | null; openTrace: () => void; openRoutine: (id: string | null) => void; openBotChat: (peerId: string) => void }) {
  const store = useStore()
  if (item.kind === 'separator') {
    return <TimeSeparator date={item.date} />
  }
  if (item.kind === 'exchanges') return <BotMessageRow exchanges={item.exchanges} open={openBotChat} />
  if (item.kind === 'steps') return <StepsRow entries={item.entries} openTrace={openTrace} />
  if (item.kind === 'task') return <TaskCard item={item} focusId={focusId} renderRow={(row) => <Row item={row} chat={chat} openTrace={openTrace} openRoutine={openRoutine} openBotChat={openBotChat} />} />
  if (item.kind === 'topic') return <TopicCard item={item} focusId={focusId} renderRow={(row) => <Row item={row} chat={chat} openTrace={openTrace} openRoutine={openRoutine} openBotChat={openBotChat} />} />
  if (item.kind === 'ending') return chat ? <EndLine botId={chat.id} text={item.text} more={item.more} /> : null
  const e = item.entry
  if (e.kind === 'notice' && e.data.routineId) {
    return (
      <button className="chat-routine-notice" onClick={() => openRoutine(e.data.routineId!)}>
        <Icon name="clock.arrow.circlepath" size={10} />
        {e.data.text ?? 'Routine'}
      </button>
    )
  }
  return <ChatRow entry={e} groupStart={item.groupStart} chat={chat} openTrace={openTrace} lateQuote={lateQuoteOf(store, e)} />
}

/** The start of an empty group chat: who's in it and how the room works. */
function GroupIntroCard({ group }: { group: Bot }) {
  const store = useStore()
  const members = store.members(group)
  const names = members.map((m) => m.name)
  return (
    <div className="intro-card">
      <GroupAvatar members={members} size={72} />
      <Text as="div" size="title2" weight="semibold" tone="text">{group.name}</Text>
      {group.name !== names.join(', ') ? <Text as="div" size="subheadline" tone="secondary">{names.join(' · ')}</Text> : null}
      {group.description ? <Text as="div" size="subheadline" tone="secondary">{group.description}</Text> : null}
      <Text as="div" size="callout" tone="secondary" className="intro-card-hint balanced">Everyone answers in turn. @mention a bot to ask just that one.</Text>
    </div>
  )
}

function IntroCard({ bot }: { bot: Bot }) {
  const store = useStore()
  return (
    <div className="intro-card">
      <BotAvatar bot={bot} size={72} />
      <Text as="div" size="title2" weight="semibold" tone="text">{bot.name}</Text>
      <Text as="div" size="callout" tone="secondary" className="intro-card-path">
        {bot.managedWorkspace ? `${store.backendName(bot.backend)} · Personal workspace` : `${store.backendName(bot.backend)} in ${bot.cwd}`}
      </Text>
      {bot.description ? <Text as="div" size="subheadline" tone="secondary">{bot.description}</Text> : null}
      <Text as="div" size="callout" tone="secondary" className="intro-card-hint">Tell it what you need. You'll get a notification when it's done or needs you.</Text>
    </div>
  )
}
