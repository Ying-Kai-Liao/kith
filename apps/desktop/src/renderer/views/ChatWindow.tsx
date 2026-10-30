import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { GrantedAgentsChat } from './computers/GrantedAgentsChat'
import { InviteJoinSheet } from './settings/InviteJoinSheet'
import { ShareSheet } from './ShareSheet'
import type { WindowCommand } from '@shared/ipc'
import { isSSHConnecting, showsChat as shouldShowChat } from '@shared/ssh-startup'
import { draftOf, isGroup, isWorking, type Bot, type BotDraft } from '@shared/models'
import { LiveBot } from '../components/Avatar'
import { Icon } from '../components/Icon'
import { ChoicePicker, ResizeHandle } from '../components/Controls'
import { Dialog, ModalHeader, Sheet, type MenuItem } from '../components/Overlay'
import { HStack } from '../components/Stack'
import { Text } from '../components/Text'
import { SHOW_COMPUTER_EVENT } from '../lib/computer-event'
import { useModels } from '../lib/observable'
import { prefs, stepTextSize, usePref, DEFAULT_TEXT_SIZE } from '../lib/prefs'
import { useApp, StoreContext } from '../store/context'
import { refKey, sameRef, parseRef, type BotRef } from '../store/app-model'
import type { BotStore } from '../store/bot-store'
import { useComputerRoster } from '../store/computer-roster'
import { CrewSidebar, RosterEmpty } from './crew/CrewSidebar'
import { ComputerFilterHeader, computerSelection } from './ComputerFilterHeader'
import { ThreadView } from './thread/ThreadView'
import { canShareAgent, ShareAgentSheet } from './ShareAgentSheet'
import { peopleFor } from '../store/people-store'
import { NewChatView } from './bots/NewChatView'
import { newChatMenu } from '../lib/new-chat-menu'
import { BotEditorView } from './bots/BotEditorView'
import { MarketplaceView } from './marketplace/MarketplaceView'
import type { SettingsPage } from '@shared/settings-pages'
import { openProfileMenu, openSettingsWindow } from './settings/profile-menu'
import { ApprovalSheet } from './settings/ApprovalSheet'
import { SearchPalette } from './SearchPalette'
import { AccountWelcomeView } from './AccountWelcomeView'
import { ComputerAccessView } from './computer-access/ComputerAccessView'
import { HostStateView } from './HostStateView'
import { StartupRecovery } from './StartupRecovery'
export { EmptyState } from './HostStateView'
import { StarPrompt } from './StarPrompt'
import { OpenVoiceSettings } from './call/CallView'
import { IncomingCalls } from './call/IncomingCall'
import { DesktopActionMenu } from './PanelMenus'
import { useTraySummary } from './tray-summary'
import { useIslandSummary } from './island-summary'
import { useSSH, useSSHAttachments } from './settings/ssh-model'
import './chat-window.css'
import './sidebar-header.css'

/** Native desktop chat: roster on the left, the conversation on the right (Grok Bot's desktop layout). */
export function ChatWindow() {
  const app = useApp()
  const [onboarded, setOnboarded] = usePref(prefs.onboardingDone)
  const [accessSetup, setAccessSetup] = usePref(prefs.computerAccessSetup)
  const [showAccess, setShowAccess] = useState(false)
  const [confirmReset, setConfirmReset] = useState(false)
  useSSHAttachments(app)
  const ssh = useSSH()
  const showsChat = shouldShowChat(app.host.state.kind, app.computers.length, ssh.state)

  // The chat's "Show the computer" opens Computer access, the Mac app's screen view.
  useEffect(() => {
    const show = () => setShowAccess(true)
    window.addEventListener(SHOW_COMPUTER_EVENT, show)
    return () => window.removeEventListener(SHOW_COMPUTER_EVENT, show)
  }, [])

  useEffect(
    () =>
      window.kith.app.onCommand((c: WindowCommand) => {
        switch (c.kind) {
          case 'computerAccess':
            return setShowAccess(true)
          case 'confirmReset':
            return setConfirmReset(true)
          case 'reviewApprovals':
            return app.reviewApprovals()
          case 'reviewBotRequests':
            // The cards sit at the top of the sidebar's Needs your OK; the window coming forward is enough.
            return
          case 'setTextSize':
            return prefs.textSize.set(c.size)
          case 'stepTextSize':
            return prefs.textSize.set(c.up === null ? DEFAULT_TEXT_SIZE : stepTextSize(prefs.textSize.get(), c.up))
          case 'setUsageIconStyle':
            return prefs.usageIconStyle.set(c.style)
          case 'openBot': {
            const ref = parseRef(c.ref)
            if (ref) {
              app.select(ref)
              // A bot detail's conversation: the chat opens with its thread.
              if (c.thread) app.store(ref.computerId)?.requestThread(ref.botId, c.thread)
            }
            return
          }
          case 'stopBot': {
            const ref = parseRef(c.ref)
            if (ref) app.store(ref.computerId)?.stop(ref.botId)
            return
          }
          case 'setRemoteScreen':
            void app.setRemoteScreen(c.on)
            return
          case 'respondPermission': {
            const ref = parseRef(c.ref)
            const store = ref ? app.store(ref.computerId) : null
            const entry = ref ? store?.allEntries(ref.botId).find((e) => e.id === c.entryId) : undefined
            if (store && entry) store.respond(entry, c.optionId)
            return
          }
          case 'sendMessage': {
            const ref = parseRef(c.ref)
            if (ref) app.store(ref.computerId)?.send(c.text, ref.botId)
            return
          }
        }
      }),
    [app],
  )

  const approval = app.currentApproval
  return (
    <div className="chat-window">
      <Summaries />
      <IncomingCalls />
      {onboarded ? (
        accessSetup === null ? (
          <ComputerAccessView onContinue={(ready) => setAccessSetup(ready ? 'ready' : 'later')} />
        ) : showsChat ? (
          <ChatSplitView />
        ) : (
          <HostStateView />
        )
      ) : (
        <AccountWelcomeView
          onContinue={() => {
            setOnboarded(true)
            app.local?.track('onboarding_completed')
          }}
        />
      )}
      {onboarded && accessSetup !== null && showsChat ? null : <div className="window-drag drag" />}
      {onboarded && accessSetup !== null ? <StarPrompt /> : null}
      <Sheet open={showAccess} onClose={() => setShowAccess(false)} width={660} height={Math.min(820, window.innerHeight - 80)}>
        <ModalHeader title="Computer access" />
        <ComputerAccessView onContinue={(ready) => { setAccessSetup(ready ? 'ready' : 'later'); setShowAccess(false) }} />
      </Sheet>
      <Dialog
        open={confirmReset}
        title="Reset all data?"
        message="Signs out every account and deletes this computer's bots, conversations, keys and settings. Kith then starts over from the welcome screen."
        actions={[{ title: 'Reset everything', destructive: true, action: () => void window.kith.app.resetAllData() }]}
        onClose={() => setConfirmReset(false)}
      />
      {/* Approving a device: the code the device shows must match (spec §4.2 B). Closing puts it off. */}
      <InviteJoinSheet />
      <ShareSheet />
      <Sheet open={approval !== null} onClose={() => approval && app.deferApproval(approval)}>
        {approval ? <ApprovalSheet key={approval.id} approval={approval} /> : null}
      </Sheet>
    </div>
  )
}

/** Keeps the tray and the approval island current: it follows every store, so it renders nothing. */
function Summaries() {
  const app = useApp()
  useTraySummary(app)
  useIslandSummary(app)
  return null
}

interface BotTarget {
  bot: Bot
  store: BotStore
}

interface EditTarget {
  id: string
  draft: BotDraft
  store: BotStore
}

/** The collapsed sidebar: the picture, new chat, the bots at 36. */
const RAIL_WIDTH = 68

function ChatSplitView() {
  const app = useApp()
  const ssh = useSSH()
  const connecting = ssh.state.profiles.some((p) => isSSHConnecting(ssh.status(p.id))) || [...app.stores.values()].some((s) => s.shownConnection.kind === 'connecting')
  const [compact, setCompact] = usePref(prefs.sidebarCompact)
  const [sidebarWidth, setSidebarWidth] = usePref(prefs.sidebarWidth)
  const [hidden, setHidden] = usePref(prefs.hiddenComputers)
  const [searching, setSearching] = useState(false)
  const [composing, setComposing] = useState(false)
  const [composingGroup, setComposingGroup] = useState(false)
  const [composeComputer, setComposeComputer] = useState<string | null>(null)
  const previousSelection = useRef<BotRef | null>(null)
  const [contextBot, setContextBot] = useState<{ target: BotTarget; x: number; y: number } | null>(null)
  const [editing, setEditing] = useState<EditTarget | null>(null)
  const [confirmDelete, setConfirmDelete] = useState<BotTarget | null>(null)
  const [newSessionBot, setNewSessionBot] = useState<BotTarget | null>(null)
  const [sharingBot, setSharingBot] = useState<BotTarget | null>(null)
  const [, bumpMenu] = useState(0)
  const [marketplace, setMarketplace] = useState<string | null>(null)
  // Settings is its own window: every old entry point opens it on the page it wants.
  const setSettings = useCallback((page: SettingsPage) => openSettingsWindow(page), [])
  const openVoiceSettings = useCallback(() => setSettings('voice'), [setSettings])
  const [windowSize, setWindowSize] = useState({ width: window.innerWidth, height: window.innerHeight })
  const searchRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  useModels([...app.stores.values()])

  useEffect(() => {
    const onResize = () => {
      setWindowSize({ width: window.innerWidth, height: window.innerHeight })
      setSidebarWidth(Math.min(prefs.sidebarWidth.get(), Math.max(260, window.innerWidth - 420)))
    }
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }, [setSidebarWidth])

  const sheetHeight = Math.max(420, windowSize.height - 76)
  const stores = app.computers.map((c) => app.store(c.id)).filter((s): s is BotStore => !!s)
  const shown = new Set(computerSelection(app.computers.map((c) => c.id), hidden).shown)
  const onlineStores = stores.filter((s) => shown.has(s.computer.id) && s.connection.kind === 'online' && !s.mismatch)
  const selectedStore = app.selectedStore
  const composeStore = composeComputer ? app.store(composeComputer) : null
  const roster = useComputerRoster(app, shown)
  const visibleRoster = roster.items

  const compose = (group = false) => {
    setComposingGroup(group)
    // The selected bot's computer if it's online, else this computer, else any online one.
    const target = [selectedStore, app.local].find((s) => s && onlineStores.includes(s)) ?? onlineStores[0]
    if (!target) return
    if (!composing) previousSelection.current = app.selection
    setComposeComputer(target.computer.id)
    app.select(null)
    setComposing(true)
  }

  useEffect(() => {
    if (app.selection) setComposing(false)
  }, [app.selection])

  useEffect(
    () =>
      window.kith.app.onCommand((c) => {
        if (c.kind === 'newChat' && onlineStores.length) compose()
        // ⌘F: the sidebar's box, or the palette while only the rail shows.
        if (c.kind === 'search') {
          if (prefs.sidebarCompact.get()) setSearching(true)
          else searchRef.current?.select()
        }
        if (c.kind === 'toggleSidebar') setCompact(!prefs.sidebarCompact.get())
      }),
  )

  // Screenshot/UI checks (development builds): open a screen at launch.
  const debugOpened = useRef(false)
  useEffect(() => {
    const target = window.kith.debugOpen
    if (!target || debugOpened.current) return
    if (target === 'compose' || target === 'group') {
      if (!onlineStores.length) return
      debugOpened.current = true
      compose(target === 'group')
    } else if (target === 'plugins') {
      if (!app.local) return
      debugOpened.current = true
      setMarketplace(app.local.computer.id)
    } else if (target === 'computers') {
      debugOpened.current = true
      setSettings('computers')
    } else {
      const item = app.roster.find((i) => i.bot.name === target)
      if (!item) return
      debugOpened.current = true
      app.select(item.ref)
    }
  })

  const newItems = (): MenuItem[] => newChatMenu({
    agent: () => compose(),
    person: () => setSettings('people'),
    bot: () => setSettings('bots'),
    group: () => compose(true),
  })
  const filterNeeded = app.computers.length > 1 || ssh.state.profiles.length > 0 || stores.some((s) => s.shownConnection.kind !== 'online')

  const openMarketplace = () => setMarketplace(([selectedStore, app.local, ...onlineStores].find((s) => s?.isOwner)?.computer.id) ?? null)

  const botActions = (bot: Bot, store: BotStore): MenuItem[] => bot.botGuest ? [
    // An outside bot's chat: read along and mark read; nothing that edits, restarts or deletes it.
    { title: 'Mark as Read', icon: 'bell.badge', action: () => store.markAllRead(bot.id) },
  ] : store.isOwner ? [
    { title: bot.pinned ? 'Unpin' : 'Pin', icon: 'pin', action: () => store.setPinned(bot, !bot.pinned) },
    { title: 'Mark as Read', icon: 'bell.badge', action: () => store.markAllRead(bot.id) },
    ...(canShareAgent(store, bot) ? [{ title: 'Share access…', icon: 'person.crop.circle.badge.plus', action: () => setSharingBot({ bot, store }) }] : []),
    // Agents only: the colour lives in the profile editor; Sleep stops what the agent is running.
    ...(isGroup(bot) ? [] : [
      { title: 'Colour…', icon: 'paintpalette', divider: true, action: () => setEditing({ id: crypto.randomUUID(), draft: draftOf(bot), store }) },
      { title: 'Sleep', icon: 'moon', disabled: !isWorking(bot), action: () => store.stop(bot.id) },
    ]),
    { title: 'Edit Profile…', icon: 'square.and.pencil', divider: isGroup(bot), action: () => setEditing({ id: crypto.randomUUID(), draft: draftOf(bot), store }) },
    { title: 'Copy conversation ID', icon: 'square.on.square', divider: true, action: () => window.kith.app.copy(bot.id) },
    { title: 'New session', icon: 'arrow.counterclockwise', action: () => setNewSessionBot({ bot, store }) },
    { title: 'Hide from sidebar', icon: 'eye.slash', divider: true, action: () => store.setHidden(bot, true) },
    { title: 'Delete', icon: 'trash', destructive: true, action: () => setConfirmDelete({ bot, store }) },
  ] : [
    // Someone else's Mac: the host lets an invited person read and mark read, nothing that edits the bot.
    { title: 'Mark as Read', icon: 'bell.badge', action: () => store.markAllRead(bot.id) },
  ]

  // ↑/↓ in the roster move the selection; Alt+↑/↓ moves the selected bot itself.
  const onListKey = (e: React.KeyboardEvent) => {
    if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return
    if (e.target instanceof HTMLElement && e.target.closest('.computer-roster-heading')) return
    if (e.altKey) {
      e.preventDefault()
      if (app.selection) roster.moveBot(app.selection, e.key === 'ArrowUp' ? -1 : 1)
      return
    }
    const refs = roster.navigable.map((i) => i.ref)
    if (!refs.length) return
    e.preventDefault()
    const current = refs.findIndex((r) => sameRef(r, app.selection))
    const next = e.key === 'ArrowDown' ? Math.min(current + 1, refs.length - 1) : Math.max(current - 1, 0)
    app.select(refs[next]!)
  }

  // The sidebar's edge drags its width; narrower than 180 collapses it to the rail.
  const [resizing, setResizing] = useState(false)
  const sidebarMax = Math.min(420, window.innerWidth - 420)
  const resizeSidebar = (proposed: number) => {
    const nextCompact = proposed < 180
    if (nextCompact !== compact) setCompact(nextCompact)
    if (!nextCompact) setSidebarWidth(Math.min(Math.max(260, proposed), sidebarMax))
  }

  const errorMessage = app.errorMessage
  const mismatchStores = stores.filter((s) => shown.has(s.computer.id) && s.mismatch)
  const selected = app.selection
  const selectedBot = selected && selectedStore?.bots.get(selected.botId)

  let detail: ReactNode
  if (composing && composeStore) {
    detail = (
      <div className="detail-column" key={`${composeStore.computer.id}-${composingGroup}`}>
        {onlineStores.length > 1 ? (
          <ComputerPicker stores={onlineStores} value={composeStore.computer.id} onChange={setComposeComputer} />
        ) : null}
        <StoreContext.Provider value={composeStore}>
          {composeStore.isOwner ? (
            <NewChatView
              group={composingGroup}
              onClose={() => {
                setComposing(false)
                if (!app.selection) app.select(previousSelection.current)
              }}
            />
          ) : (
            <GrantedAgentsChat onClose={() => setComposing(false)} />
          )}
        </StoreContext.Provider>
      </div>
    )
  } else if (selected && selectedStore && selectedBot) {
    detail = (
      <StoreContext.Provider value={selectedStore}>
        <ThreadView key={refKey(selected)} botId={selected.botId} />
      </StoreContext.Provider>
    )
  } else if (!onlineStores.length && ssh.state.profiles.length) {
    detail = <StartupRecovery manage={() => setSettings('computers')} />
  } else {
    detail = (
      <div className="empty-detail">
        <div className="detail-drag drag" />
        <div className="empty-detail-crew kith-bot-shadow">
          <LiveBot face={{ kind: 'crew', name: 'builder' }} size={74} id="builder" mood="working" />
          <LiveBot face={{ kind: 'crew', name: 'designer' }} size={92} id="designer" />
          <LiveBot face={{ kind: 'crew', name: 'coder' }} size={74} id="coder" mood="working" />
        </div>
        <p className="empty-detail-title">{connecting ? 'Connecting to your computers…' : 'Your coding agents, as teammates.'}</p>
        <p className="empty-detail-message">{connecting ? 'Your crew will appear once connected. Manage computers to see connection progress.' : 'Pick someone on the left, or ask the crew below.'}</p>
        <button className="empty-detail-new" onClick={() => compose()} disabled={!onlineStores.length}>
          <Icon name="square.and.pencil" size={13} weight="medium" />
          New agent
        </button>
      </div>
    )
  }

  const width = compact ? RAIL_WIDTH : sidebarWidth
  return (
    <div className={`split ${contextBot ? 'covered' : ''}`}>
      <div className="kith-sky-header window-sky" aria-hidden />
      <aside className={`sidebar ${compact ? 'compact' : ''} ${resizing ? 'resizing' : ''}`} style={{ width }}>
        <CrewSidebar
          compact={compact}
          roster={roster}
          items={visibleRoster}
          listRef={listRef}
          onListKey={onListKey}
          onContext={(bot, store, x, y) => {
            setContextBot({ target: { bot, store }, x, y })
            // The host's answer about people decides whether Share shows; ask once if it hasn't been.
            if (peopleFor(store).supported === null) void peopleFor(store).load().then(() => bumpMenu((n) => n + 1))
          }}
          searchRef={searchRef}
          newItems={newItems}
          canCreate={onlineStores.length > 0}
          accountOpen={false}
          onAccount={() => void openProfileMenu(app, { onExtensions: openMarketplace })}
          onExtensions={openMarketplace}
          filter={filterNeeded ? <ComputerFilterHeader hidden={hidden} setHidden={setHidden} manage={() => setSettings('computers')} compact /> : null}
          emptyNote={visibleRoster.length === 0 && !roster.grouped && mismatchStores.length === 0 ? <RosterEmpty online={onlineStores.length > 0} /> : null}
        />
      </aside>
      <ResizeHandle label="Resize sidebar" width={width} min={RAIL_WIDTH} max={sidebarMax} edge="trailing" onResize={resizeSidebar} onDragging={setResizing} />
      <main className="detail">
        {/* The call bar's settings open this window's Settings at Voice chat. */}
        <OpenVoiceSettings.Provider value={openVoiceSettings}>{detail}</OpenVoiceSettings.Provider>
      </main>

      {contextBot ? (
        <div className="context-layer" onMouseDown={(e) => e.target === e.currentTarget && setContextBot(null)}>
          <DesktopActionMenu
            items={botActions(contextBot.target.bot, contextBot.target.store)}
            onDismiss={() => setContextBot(null)}
            style={{ left: Math.min(Math.max(8, contextBot.x), windowSize.width - 268), top: Math.min(Math.max(8, contextBot.y), Math.max(8, windowSize.height - 344)) }}
          />
        </div>
      ) : null}

      <SearchPalette open={searching} items={visibleRoster} onPick={(ref) => app.select(ref)} onClose={() => setSearching(false)} />
      <Sheet open={editing !== null} onClose={() => setEditing(null)} width={520} height={Math.min(680, sheetHeight)}>
        {editing ? (
          <StoreContext.Provider value={editing.store}>
            <BotEditorView key={editing.id} draft={editing.draft} />
          </StoreContext.Provider>
        ) : null}
      </Sheet>
      <Sheet open={marketplace !== null} onClose={() => setMarketplace(null)} width={Math.min(920, windowSize.width - 80)} height={sheetHeight} dismissOnDim>
        {marketplace && app.store(marketplace) ? (
          <StoreContext.Provider value={app.store(marketplace)!}>
            <MarketplaceView appsFirst key={marketplace} computers={onlineStores.map((s) => ({ id: s.computer.id, name: s.hostName }))} computer={marketplace} onComputer={setMarketplace} />
          </StoreContext.Provider>
        ) : null}
      </Sheet>
      {sharingBot ? <ShareAgentSheet store={sharingBot.store} bot={sharingBot.bot} open onClose={() => setSharingBot(null)} /> : null}
      <Dialog
        open={newSessionBot !== null}
        title="Start a new session?"
        message="The conversation stays here, but the agent starts with a fresh context."
        actions={newSessionBot ? [{ title: 'New session', action: () => newSessionBot.store.newSession(newSessionBot.bot.id) }] : []}
        onClose={() => setNewSessionBot(null)}
      />
      <Dialog
        open={confirmDelete !== null}
        title={`Delete ${confirmDelete?.bot.name ?? 'agent'}?`}
        message="Files it changed on your computer stay as they are."
        actions={confirmDelete ? [{ title: 'Delete bot and its conversation', destructive: true, action: () => confirmDelete.store.delete(confirmDelete.bot) }] : []}
        onClose={() => setConfirmDelete(null)}
      />
      <Dialog open={errorMessage !== null} title="Something went wrong" message={errorMessage} actions={[]} cancel="OK" onClose={() => app.clearErrors()} />
    </div>
  )
}

/** Which computer a new chat goes to, when more than one is online. */
function ComputerPicker({ stores, value, onChange }: { stores: BotStore[]; value: string; onChange: (id: string) => void }) {
  const options = useMemo(() => stores.map((s) => ({ id: s.computer.id, label: s.hostName })), [stores])
  return (
    <HStack gap={8} className="compose-computer">
      <Text tone="secondary">On</Text>
      <ChoicePicker selection={value} options={options} onChange={onChange} />
    </HStack>
  )
}
