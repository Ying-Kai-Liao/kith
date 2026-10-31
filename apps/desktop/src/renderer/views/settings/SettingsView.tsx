import { useEffect, useState } from 'react'
import { islandLabel } from '@shared/island'
import { parseAppearance } from '@shared/appearance'
import { isSettingsPage, resolveSettingsPage, type SettingsPage } from '@shared/settings-pages'
import { pagesFor, type ComputerRole } from '@shared/people'
import { cloudRole } from '@shared/relay'
import { Button, CardSection, ChoicePicker, IconButton, ProgressBar, SegmentedChoice, Switch, ValueRow } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { ModalHeader, Sheet } from '../../components/Overlay'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useModel, useModels } from '../../lib/observable'
import { DEFAULT_TEXT_SIZE, prefs, TEXT_SIZES, usePref } from '../../lib/prefs'
import { useAccount } from '../../store/account'
import { StoreContext, useApp } from '../../store/context'
import { botGuestsFor } from '../../store/bot-guests-store'
import { peopleFor } from '../../store/people-store'
import { UpdateButton, useUpdates } from '../UpdateButton'
import { AccountDevices } from './ComputersView'
import { SkyPatch } from '../../components/kith'
import { MyBotsPage } from './BotGuestsView'
import { PeopleView } from './PeopleView'
import { ProfileImagePicker } from './ProfileImagePicker'
import { KeysView } from './KeysView'
import { LinkChatRow, ReplyCardsRow, ScheduleMessagesRow } from './LinkChatRow'
import { errorText, isMac, thisMac } from './parts'
import { BackupChains } from './BackupChain'
import { UsageLimits } from './UsageLimits'
import { SpendSection } from '../usage/SpendSection'
import { VoiceChatSettingsView } from './VoiceChatSettingsView'
import { FormSheet } from '../marketplace/pieces'
import { ComputerAccessView } from '../computer-access/ComputerAccessView'
import { AnalyticsSheet } from './AnalyticsSheet'
import { LicensesSheet } from './LicensesSheet'
import { ApprovalSheet } from './ApprovalSheet'
import { useSSHAttachments } from './ssh-model'
import './settings-window.css'

export type { SettingsPage }

// Account is the sidebar's header, not a row; `computers` is a page name that opens it (see resolveSettingsPage).
const pages: { id: SettingsPage; title: string; icon: string; gapAfter?: boolean }[] = [
  { id: 'account', title: 'Account', icon: 'person.crop.circle' },
  { id: 'crew', title: 'Crew', icon: 'person.2' },
  { id: 'people', title: 'People', icon: 'person.2' },
  { id: 'keys', title: 'Keys', icon: 'key' },
  { id: 'bots', title: 'Your bots', icon: 'bubble.left.and.bubble.right' },
  { id: 'voice', title: 'Voice chat', icon: 'waveform' },
  { id: 'usage', title: 'Usage', icon: 'gauge.with.dots.needle.33percent', gapAfter: true },
  { id: 'notifications', title: 'Notifications & display', icon: 'bell.badge' },
  { id: 'privacy', title: 'Privacy', icon: 'lock.shield', gapAfter: true },
  { id: 'updates', title: 'Updates', icon: 'arrow.down.circle' },
  { id: 'about', title: 'About', icon: 'info.circle' },
]

/** Pages only the owner of the connected Mac has: an invited person keeps Account and what is theirs alone. */
const ownerOnly: SettingsPage[] = ['people', 'keys', 'voice', 'usage', 'updates']

/** This account's part in the connected Mac, from the cloud's computer rows; the owner when it isn't listed (the Mac's own app). */
export function useComputerRole(): ComputerRole {
  const app = useApp()
  const id = app.local?.computer.id
  const row = app.cloud.computers.find((c) => c.computerId === id)
  return row ? cloudRole(row.role) : 'owner'
}

/** The Settings window (⌘,): a sidebar of pages and a small centred title over grouped cards. */
export function SettingsWindow({ initial }: { initial: SettingsPage }) {
  const [page, setPage] = useState(resolveSettingsPage(initial))
  const app = useApp()
  const account = useAccount()
  useSSHAttachments(app)
  const role = useComputerRole()
  // A host before invites answers "unknown method": People hides rather than failing.
  const people = useModel(app.local ? peopleFor(app.local) : null)
  const online = app.local?.connection.kind === 'online'
  useEffect(() => void (online && role === 'owner' && people?.supported === null ? people.load() : undefined), [online, role, people, people?.supported])
  // The menu and ⌘, ask an open window to show a page.
  useEffect(() => window.kith.app.onCommand((c) => c.kind === 'openSettingsPage' && isSettingsPage(c.page) && setPage(resolveSettingsPage(c.page))), [])
  // Your bots is the invitee's page: an owner manages bots under People. It shows when a Mac they are a member of answers.
  const memberBots = [...app.stores.values()].filter((s) => botGuestsFor(s).mode === 'member')
  useModels(memberBots.map(botGuestsFor))
  useEffect(() => void memberBots.forEach((s) => void botGuestsFor(s).load()), [memberBots.length]) // eslint-disable-line react-hooks/exhaustive-deps
  const hasBots = memberBots.some((s) => botGuestsFor(s).supported === true)
  const visible = pagesFor(role, pages.map((p) => p.id), ownerOnly).filter((id) => (id !== 'people' || people?.supported === true) && (id !== 'bots' || hasBots))
  const shown = pages.filter((p) => visible.includes(p.id) && p.id !== 'account')
  const current = shown.find((p) => p.id === page) ?? pages[0]!
  const approval = app.currentApproval
  return (
    <div className="settings-win">
      <nav className="settings-win-sidebar">
        <div className="settings-win-drag drag" />
        <button className={`settings-win-account ${current.id === 'account' ? 'selected' : ''}`} aria-current={current.id === 'account' ? 'page' : undefined} onClick={() => setPage('account')}>
          {account.user?.avatarURL ? (
            <img src={account.user.avatarURL} alt="" className="settings-win-avatar" />
          ) : (
            <SkyPatch id={account.user?.userId ?? 'you'} you={!account.user} size={32} />
          )}
          <span className="settings-win-account-text">
            <span className="settings-win-account-name">{account.isSignedIn ? (account.user?.email ?? 'Signed in') : 'Not signed in'}</span>
            <span className="settings-win-account-sub">{account.isSignedIn ? 'Kith account' : 'Sign in'}</span>
          </span>
          {app.approvals.length > 0 ? <span className="settings-count">{app.approvals.length}</span> : null}
        </button>
        {shown.map((item) => (
          <button key={item.id} className={`settings-win-item ${item.id === current.id ? 'selected' : ''} ${item.gapAfter ? 'gap-after' : ''}`} aria-current={item.id === current.id ? 'page' : undefined} onClick={() => setPage(item.id)}>
            <Icon name={item.icon} size={13} />
            <span className="settings-win-item-label">{item.title}</span>
          </button>
        ))}
      </nav>
      <main className="settings-win-main">
        <header className="settings-win-title drag">{current.title}</header>
        <div key={current.id} className="settings-win-page">
          <div className="settings-win-content">
            <PageContent page={current.id} />
          </div>
        </div>
      </main>
      <Sheet className="settings-sheet" open={approval !== null} onClose={() => approval && app.deferApproval(approval)}>
        {approval ? <ApprovalSheet key={approval.id} approval={approval} /> : null}
      </Sheet>
    </div>
  )
}

function PageContent({ page }: { page: SettingsPage }) {
  switch (page) {
    case 'account':
      return <AccountSettings />
    case 'crew':
      return <CrewSettings />
    case 'computers':
      return <AccountSettings />
    case 'people':
      return <PeopleView />
    case 'keys':
      return <KeysView />
    case 'bots':
      return <MyBotsPage />
    case 'voice':
      return <VoiceSettings />
    case 'usage':
      return <UsageSettings />
    case 'notifications':
      return <NotificationSettings />
    case 'privacy':
      return <PrivacySettings />
    case 'updates':
      return <UpdateSettings />
    case 'about':
      return <AboutSettings />
  }
}

/** Who is signed in and how to sign in or out; then this account's computers. */
function AccountSettings() {
  const account = useAccount()
  const role = useComputerRole()
  const [picking, setPicking] = useState(false)
  const avatar = account.user?.avatarURL ? (
    <img src={account.user.avatarURL} alt="" className="settings-identity-avatar" />
  ) : (
    <SkyPatch id={account.user?.userId ?? 'you'} you={!account.user} size={48} />
  )
  return (
    <>
      <div className="settings-identity">
        {account.isSignedIn ? (
          <button className="settings-identity-change" aria-label="Change photo" title="Change photo" onClick={() => setPicking(true)}>
            {avatar}
          </button>
        ) : (
          avatar
        )}
        <VStack grow className="settings-stack">
          <Text truncate weight="semibold">{account.isSignedIn ? (account.user?.email ?? 'Signed in') : 'Not signed in'}</Text>
          <Text size="caption" tone="secondary">
            {account.isSignedIn ? 'Reach your computers from anywhere' : 'Sign in to keep one crew on every Mac.'}
          </Text>
        </VStack>
        {account.isSignedIn ? (
          <>
            <Button kind="secondary" onClick={() => setPicking(true)} disabled={account.isBusy}>Change photo</Button>
            <Button kind="secondary" onClick={() => void account.signOut()} disabled={account.isBusy}>
              {account.isBusy ? 'Please wait…' : 'Sign out'}
            </Button>
          </>
        ) : (
          <>
            {account.appleSignIn ? <Button kind="secondary" onClick={() => void account.signIn('apple')} disabled={account.isBusy}>Apple</Button> : null}
            <Button kind="secondary" onClick={() => void account.signIn('google')} disabled={account.isBusy}>Google</Button>
            <Button kind="secondary" onClick={() => void account.signIn('github')} disabled={account.isBusy}>GitHub</Button>
          </>
        )}
      </div>
      {account.errorMessage ? <Text as="div" size="caption" tone="attention">{account.errorMessage}</Text> : null}
      <AccountDevices owner={role === 'owner'} />
      <Sheet className="settings-sheet" open={picking && account.isSignedIn} onClose={() => setPicking(false)} width={420} dismissOnDim>
        <ProfileImagePicker onClose={() => setPicking(false)} />
      </Sheet>
    </>
  )
}

/** Crew order, linked chats and backups; then the display and system preferences. */
function CrewSettings() {
  const [crewOrder, setCrewOrder] = usePref(prefs.crewOrder)
  const [textSize, setTextSize] = usePref(prefs.textSize)
  const [appearance, setAppearance] = usePref(prefs.appearance)
  const [usageIconStyle, setUsageIconStyle] = usePref(prefs.usageIconStyle)
  const [compact, setCompact] = usePref(prefs.sidebarCompact)
  const [showInDock, setShowInDock] = useState(true)
  useEffect(() => void window.kith.app.showInDock().then(setShowInDock), [])
  const [launchAtLogin, setLaunchAtLogin] = useState(false)
  useEffect(() => void window.kith.app.launchAtLogin().then(setLaunchAtLogin), [])
  const [island, setIsland] = useState(false)
  useEffect(() => void window.kith.island.enabled().then(setIsland), [])
  const [backups, setBackups] = useState(false)

  return (
    <>
      <CardSection title="Crew">
        <ValueRow label="Crew order" detail="pinned first, then needs you, working, recent">
          <ChoicePicker selection={crewOrder} options={[{ id: 'automatic', label: 'Automatic' }, { id: 'manual', label: 'Manual' }]} onChange={setCrewOrder} />
        </ValueRow>
        <LinkChatRow />
        <ScheduleMessagesRow />
        <ReplyCardsRow />
        <button className="settings-row settings-nav-row" onClick={() => setBackups(true)}>
          <VStack grow className="settings-stack">
            <span>Backups</span>
            <Text size="caption" tone="secondary">fallback accounts when a limit is hit</Text>
          </VStack>
          <Icon name="chevron.right" size={11} color="var(--secondary)" />
        </button>
      </CardSection>
      <CardSection title="Display">
        <ValueRow label="Text size">
          <ChoicePicker
            selection={textSize}
            options={TEXT_SIZES.map((s) => ({ id: s, label: s === DEFAULT_TEXT_SIZE ? `${s} pt (Default)` : `${s} pt` }))}
            onChange={setTextSize}
          />
        </ValueRow>
        <ValueRow label="Appearance">
          <ChoicePicker
            selection={parseAppearance(appearance)}
            options={[{ id: 'system', label: 'System (Default)' }, { id: 'light', label: 'Light' }, { id: 'dark', label: 'Dark' }]}
            onChange={setAppearance}
          />
        </ValueRow>
        <ValueRow label="Usage icons">
          <ChoicePicker selection={usageIconStyle} options={[{ id: 'character', label: 'Character' }, { id: 'original', label: 'Original' }]} onChange={setUsageIconStyle} />
        </ValueRow>
        <Switch on={compact} onChange={setCompact}>
          <span className="settings-stack">
            <span>Compact sidebar</span>
            <Text size="caption" tone="secondary">bots only, as a rail</Text>
          </span>
        </Switch>
      </CardSection>
      <CardSection title="System">
        {[
          ...(window.kith.island.supported
            ? [
                <Switch key="island" on={island} onChange={(on) => void window.kith.island.setEnabled(on).then(setIsland)}>
                  {islandLabel(window.kith.platform)}
                </Switch>,
              ]
            : []),
          ...(window.kith.platform === 'darwin'
            ? [<Switch key="dock" on={showInDock} onChange={(on) => void window.kith.app.setShowInDock(on).then(setShowInDock)}>Show in Dock</Switch>]
            : []),
          <Switch key="login" on={launchAtLogin} onChange={(on) => void window.kith.app.setLaunchAtLogin(on).then(setLaunchAtLogin)}>Open at login</Switch>,
        ]}
      </CardSection>
      <Sheet className="settings-sheet" open={backups} onClose={() => setBackups(false)} width={460}>
        <ModalHeader title="Backups" />
        <div className="settings-devices-body">
          <BackupChains />
        </div>
      </Sheet>
    </>
  )
}

/** Voice chat per computer: a segmented control picks the computer when there are several. */
function VoiceSettings() {
  const app = useApp()
  const stores = app.computers.flatMap((c) => app.store(c.id) ?? [])
  const [chosen, setChosen] = useState<string | null>(null)
  const store = stores.find((s) => s.computer.id === chosen) ?? app.local ?? stores[0] ?? null
  if (!store) {
    return <Text tone="secondary">Voice keys are kept by {thisMac}'s host, which isn't running.</Text>
  }
  return (
    <>
      {stores.length > 1 ? (
        <SegmentedChoice selection={store.computer.id} options={stores.map((s) => ({ id: s.computer.id, label: s.hostName }))} onChange={setChosen} />
      ) : null}
      <StoreContext.Provider value={store} key={store.computer.id}>
        <VoiceChatSettingsView showsHeader={false} />
      </StoreContext.Provider>
    </>
  )
}

/** Every computer's plan limits (a group per computer and provider), then what its agents cost at API prices. */
function UsageSettings() {
  const app = useApp()
  const stores = app.computers.flatMap((c) => app.store(c.id) ?? [])
  const withUsage = stores.filter((s) => s.usage.providers.length > 0)
  const many = stores.length > 1
  return (
    <>
      {withUsage.map((store) => <UsageLimits key={store.computer.id} usage={store.usage} computer={many ? store.hostName : null} />)}
      {stores.filter((s) => s.usage.providers.length === 0 && many).map((store) => (
        <CardSection key={store.computer.id} title={store.hostName}>
          <Text tone="secondary">No usage information yet.</Text>
        </CardSection>
      ))}
      {withUsage.length === 0 && !many ? (
        <CardSection><Text tone="secondary">No usage information yet.</Text></CardSection>
      ) : null}
      {stores.filter((s) => s.isOwner).map((store) => <SpendSection key={`spend-${store.computer.id}`} store={store} title={many ? store.hostName : null} />)}
    </>
  )
}

/** Notifications come from the system's own settings; the Crew page keeps the notch and Dock. */
function NotificationSettings() {
  return (
    <CardSection
      title="Notifications"
      footer={isMac ? 'Banners and sounds for a bot that needs you are set in System Settings.' : 'Notifications follow your system settings.'}
    >
      {isMac ? (
        <button className="settings-row settings-nav-row" onClick={() => window.kith.app.openSettings('x-apple.systempreferences:com.apple.Notifications-Settings.extension')}>
          <span className="settings-grow">Notification settings</span>
          <Icon name="arrow.up.right.square" size={11} color="var(--secondary)" />
        </button>
      ) : (
        <Text tone="secondary">Notifications follow your system settings.</Text>
      )}
    </CardSection>
  )
}

/** Product analytics on this computer's host (only the computer itself may change it), and computer access. */
function PrivacySettings() {
  const app = useApp()
  const local = useModel(app.local)
  const role = useComputerRole()
  const running = local !== null && local.connection.kind === 'online'
  const [about, setAbout] = useState(false)
  const [access, setAccess] = useState(false)
  const setShared = (on: boolean) => void local?.setAnalytics(on).catch((e: unknown) => app.setError(errorText(e)))
  return (
    <>
      <CardSection title="Privacy" footer="Which features you use, never your messages, code or files. Helps us spot bugs sooner and make Kith better for you.">
        <HStack gap={4}>
          <span className={running ? undefined : 'settings-off'}>Share usage analytics</span>
          <IconButton title="About usage analytics" icon="info.circle" onClick={() => setAbout(true)} />
          <Switch on={local?.analytics === true} onChange={setShared} disabled={!running} label="Share usage analytics" className="settings-privacy-switch" />
        </HStack>
      </CardSection>
      {role === 'owner' ? (
        <CardSection title="Computer access">
          <button className="settings-row settings-nav-row" onClick={() => setAccess(true)}>
            <VStack grow className="settings-stack">
              <span>Screen and input access</span>
              <Text size="caption" tone="secondary">lets bots see and use {thisMac}'s screen</Text>
            </VStack>
            <Icon name="chevron.right" size={11} color="var(--secondary)" />
          </button>
        </CardSection>
      ) : null}
      <FormSheet className="settings-sheet" open={about} onClose={() => setAbout(false)}>
        <AnalyticsSheet />
      </FormSheet>
      <Sheet className="settings-sheet" open={access} onClose={() => setAccess(false)} width={660} height={Math.min(820, window.innerHeight - 80)}>
        <ModalHeader title="Computer access" />
        <ComputerAccessView />
      </Sheet>
    </>
  )
}

/** The app's update check and preferences, the same as the menu bar's Updates menu. */
function UpdateSettings() {
  const updates = useUpdates()
  if (!updates) return null
  const lastChecked = updates.lastCheck
    ? `Last checked ${new Date(updates.lastCheck).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })}`
    : null
  return (
    <CardSection title="Kith" footer={updates.supported ? null : 'Automatic updates are off until Kith publishes its own releases.'}>
      {[
        <ValueRow key="version" label="Version" detail={lastChecked}>
          <HStack as="span" gap={10}>
            <span>{window.kith.appVersion}</span>
            <UpdateButton updates={updates} />
          </HStack>
        </ValueRow>,
        ...(updates.downloadPercent !== null
          ? [
              <div key="progress" className="settings-row settings-appear">
                <ProgressBar value={updates.downloadPercent / 100} label={`Downloading Kith ${updates.availableVersion ?? ''}`} />
              </div>,
            ]
          : []),
        <Switch key="check" on={updates.autoCheck} onChange={(on) => window.kith.updates.setAutoCheck(on)} disabled={!updates.supported}>
          Automatically check for updates
        </Switch>,
        <Switch key="download" on={updates.autoDownload} onChange={(on) => window.kith.updates.setAutoDownload(on)} disabled={!updates.supported}>
          Automatically download and install
        </Switch>,
        ...(updates.error
          ? [
              <div key="error" className="settings-row settings-appear">
                <Text tone="attention" grow>{updates.error}</Text>
                <Button kind="secondary" onClick={() => window.kith.updates.check()} disabled={updates.checking}>Retry</Button>
              </div>,
            ]
          : []),
      ]}
    </CardSection>
  )
}

/** The app's version, its licences and where to get help. */
function AboutSettings() {
  const [licenses, setLicenses] = useState(false)
  const open = (url: string) => window.kith.app.openExternal(url)
  return (
    <>
      <CardSection title={window.kith.appName}>
        <ValueRow label="Version">{window.kith.appVersion}</ValueRow>
        <button className="settings-row settings-nav-row" onClick={() => setLicenses(true)}>
          <span className="settings-grow">Open-source licenses</span>
          <Icon name="chevron.right" size={11} color="var(--secondary)" />
        </button>
        <button className="settings-row settings-nav-row" onClick={() => open('https://github.com/Ying-Kai-Liao/kith#readme')}>
          <span className="settings-grow">Help & documentation</span>
          <Icon name="arrow.up.right.square" size={11} color="var(--secondary)" />
        </button>
        <button className="settings-row settings-nav-row" onClick={() => open('https://github.com/Ying-Kai-Liao/kith/issues')}>
          <span className="settings-grow">Report an issue</span>
          <Icon name="arrow.up.right.square" size={11} color="var(--secondary)" />
        </button>
      </CardSection>
      <FormSheet className="settings-sheet" open={licenses} onClose={() => setLicenses(false)}>
        <LicensesSheet />
      </FormSheet>
    </>
  )
}
