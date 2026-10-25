import type { SignInProvider } from './account-config'
import type { SharedFile } from './models'
import type { SSHBridge } from './ssh'
import type { ScreenAgentState } from './computer-access'
import type { CallAnswer, IncomingPanelCall } from './incoming-call'
import type { IslandRect, IslandScreen, IslandSummary } from './island'
import type { ErrorReport } from './error-report'
import type { InviteLink } from './invite'
import type { AccessProgress, AccessTicket, CloudResult, RelayCallError, RelayComputer, RelayState } from './relay'

// The bridge between the main process (host lifecycle, loopback HTTP, tray, windows)
// and the renderer (stores and UI). Exposed as `window.kith` by the preload script.

export type HostState =
  | { kind: 'missingBinary' }
  | { kind: 'notInstalled' }
  | { kind: 'starting' }
  | { kind: 'running' }
  | { kind: 'failed'; message: string }

/** This computer's own host, once it answered. */
export interface LocalHost {
  computerId: string
  baseURL: string
  token: string
}

export interface HostSnapshot {
  state: HostState
  local: LocalHost | null
  /** Started against a manually run host (`KITH_SERVE_PORT`). */
  dev: boolean
  logPath: string
}

export interface CallError {
  status: number
  message: string
}

/** What the tray menu shows; the renderer owns the stores and sends this on every change. */
export interface TraySummary {
  status: string
  canPair: boolean
  bots: { ref: string; name: string; detail: string; working: boolean; avatar: string | null }[]
  approval: string | null
  /** Whose bot from outside waits for an allow (the person, or the bot's own name); null when none does. */
  botRequest: string | null
  usage: { name: string; icon: string | null; lines: { title: string; bar: string | null }[] }[]
  screen: {
    enabled: boolean
    subtitle: string
    error: string | null
  } | null
  version: string | null
  needsAttention: boolean
  textSize: number
  usageIconStyle: 'character' | 'original'
}

/** Things the tray menu (or the app menu) asks the window to do. */
/** What the native Profile menu shows: the renderer knows the account, the Macs and the pending requests. */
export interface ProfileMenuSpec {
  name: string
  /** "Google · 2 Macs · 2 people": only the parts the data has. */
  detail: string
  signedIn: boolean
  busy: boolean
  /** Pending access requests: a count after Settings…. */
  approvals: number
}
export type ProfileMenuChoice = 'computers' | 'extensions' | 'usage' | 'settings' | 'help' | 'issue' | 'signOut' | 'apple' | 'google' | 'github'

export type WindowCommand =
  | { kind: 'openBot'; ref: string; thread?: string }
  | { kind: 'stopBot'; ref: string }
  | { kind: 'reviewApprovals' }
  /** Bring the window forward to the cards of bots waiting to be let in. */
  | { kind: 'reviewBotRequests' }
  | { kind: 'confirmReset' }
  | { kind: 'setTextSize'; size: number }
  | { kind: 'stepTextSize'; up: boolean | null }
  | { kind: 'setUsageIconStyle'; style: 'character' | 'original' }
  | { kind: 'setRemoteScreen'; on: boolean }
  | { kind: 'computerAccess' }
  | { kind: 'newChat' }
  /** An invite link arrived; the window takes it with `invites.takePending`. */
  | { kind: 'joinInvite' }
  | { kind: 'search' }
  | { kind: 'toggleSidebar' }
  | { kind: 'toggleDetails' }
  | { kind: 'respondPermission'; ref: string; entryId: string; optionId: string }
  | { kind: 'sendMessage'; ref: string; text: string }
  /** The Settings window is already open: show this page (a SettingsPage id). */
  | { kind: 'openSettingsPage'; page: string }

/** The app's own updates (electron-updater, release builds only). */
export interface UpdateState {
  supported: boolean
  checking: boolean
  availableVersion: string | null
  /** Downloading: percent done (0–100); null when nothing is downloading. */
  downloadPercent: number | null
  /** Downloaded and ready: installs on restart. */
  staged: boolean
  /** Restarting into the staged release (the host stops first). */
  installing: boolean
  autoCheck: boolean
  autoDownload: boolean
  lastCheck: number | null
  error: string | null
  /** The release waits until paired iPhones can get the app it needs. */
  waitingForApp: string | null
}

export interface AccountState {
  /** The persisted session has been restored (or there was none). */
  ready: boolean
  /** This build has a Clerk key and a cloud. */
  configured: boolean
  busy: boolean
  error: string | null
  user: { userId: string; email: string | null; avatarURL: string | null } | null
  cloudURL: string | null
}

export type SpeechEvent =
  | { type: 'partial' | 'final'; text: string }
  | { type: 'level'; value: number }
  | { type: 'started' }
  | { type: 'stopped' }
  | { type: 'error'; message: string }

export interface KithBridge {
  /** `web`: the member-only browser build (src/renderer/web). */
  platform: 'darwin' | 'linux' | 'win32' | 'web'
  appName: string
  appScheme: string
  hostPort: number
  appVersion: string
  /** The computer's name ("Kevin's MacBook Pro"). */
  computerName: string
  /** Screenshot/UI checks: KITH_DEBUG_OPEN=compose | group | plugins | computers | <bot name>. */
  debugOpen: string | null
  host: {
    snapshot(): Promise<HostSnapshot>
    onChange(cb: (s: HostSnapshot) => void): () => void
    install(): void
    restart(): void
    /** POST /api/<method> on a loopback host. Rejects with a `CallError`. */
    call(baseURL: string, token: string, method: string, body: unknown, timeoutMs: number): Promise<unknown>
    /** SSE on a loopback host: each `data:` line, then `onEnd` (with an error when it failed). */
    stream(url: string, token: string, onData: (line: string) => void, onEnd: (error: CallError | null) => void): () => void
  }
  account: {
    state(): Promise<AccountState>
    onChange(cb: (s: AccountState) => void): () => void
    signIn(provider: SignInProvider): Promise<void>
    signOut(): Promise<void>
    /** The signed-in session's JWT for the Kith cloud. */
    token(): Promise<string>
  }
  /** On-device speech recognition (macOS: SFSpeechRecognizer in a helper; unavailable on Linux). */
  speech: {
    available: boolean
    /** The languages it can transcribe, as locale identifiers. */
    locales(): string[]
    /** `locale` null: the computer's language. */
    /** `echoCancel`: cancel what the computer plays from the microphone (for talking over a voice). */
    start(locale: string | null, options?: { echoCancel?: boolean }): void
    stop(): void
    onEvent(cb: (e: SpeechEvent) => void): () => void
  }
  ssh: SSHBridge
  /** The notch island (shared/island.ts): its setting, the chat window's summary, and the island window's side. */
  island: {
    /** False where a window can't place itself at the top of the screen (Linux on Wayland). */
    supported: boolean
    enabled(): Promise<boolean>
    setEnabled(on: boolean): Promise<boolean>
    setSummary(summary: IslandSummary): void
    state(): Promise<{ screen: IslandScreen; summary: IslandSummary }>
    onSummary(cb: (summary: IslandSummary) => void): () => void
    onScreen(cb: (screen: IslandScreen) => void): () => void
    /** The pointer reached or left the shape. */
    onHover(cb: (inside: boolean) => void): () => void
    setShape(rect: IslandRect): void
    /** The message field takes the keyboard (the island never does otherwise). */
    setFocusable(on: boolean): void
    /** ⌥Y / ⌥N answer the approval on screen, from any app, while it shows. */
    setKeys(on: boolean): void
    onKey(cb: (answer: 'allow' | 'deny') => void): () => void
    respond(ref: string, entryId: string, optionId: string): void
    send(ref: string, text: string): void
    openBot(ref: string, thread?: string): void
    newBot(): void
    openApp(): void
  }
  /**
   * The floating incoming-call panel (main/incoming-call.ts). The chat window owns the call and asks for the panel; the
   * panel window shows it and sends the answer back through the main process.
   */
  incomingCall: {
    /** Shows `call` in the panel (or a system notification in full screen); false when neither could open, so the window shows it. */
    show(call: IncomingPanelCall): Promise<boolean>
    /** The call is over (answered elsewhere, declined, timed out): closes the panel if it shows this one. */
    close(id: string): void
    /** The chat window: the person answered in the panel or the notification. */
    onAnswer(cb: (answer: { id: string; answer: CallAnswer }) => void): () => void
    /** The panel window: what it shows now, and a newer call replacing it. */
    current(): Promise<IncomingPanelCall | null>
    onCall(cb: (call: IncomingPanelCall | null) => void): () => void
    respond(id: string, answer: CallAnswer): void
  }
  /** The Kith cloud's `/v1` API with the signed-in session (and the device key for `signed`). */
  cloud: {
    request(method: string, path: string, body: unknown, signed: boolean): Promise<{ ok: true; value: unknown } | { ok: false; error: { status: number; code: string; message: string | null } }>
    /** Accepts an invite with h1 = hex sha256 of its token (never the token itself). */
    acceptInvite(inviteId: string, h1: string): Promise<{ ok: true; value: unknown } | { ok: false; error: { status: number; code: string; message: string | null } }>
  }
  /**
   * Other Macs over the Kith cloud relay (main/relay/): the end-to-end encrypted channel lives in the main process and is
   * shared by every window; `signKey` is the Mac's own key, and must hash to `computerId` or nothing connects.
   */
  relay: {
    /** The Macs this account owns or is a member of. Registers this device with the account the first time. */
    computers(): Promise<CloudResult<RelayComputer[]>>
    /** One call to the Mac's host API. Rejects with a `RelayCallError` (a host's own error comes back unchanged). */
    call(computerId: string, signKey: string, method: string, body: unknown, timeoutMs: number): Promise<unknown>
    /** `sub` "events": each element is one event's JSON; `onEnd` with an error when the channel dropped. */
    stream(computerId: string, signKey: string, sub: string, body: unknown, onData: (line: string) => void, onEnd: (error: RelayCallError | null) => void): () => void
    state(computerId: string, signKey: string): Promise<RelayState>
    onState(cb: (computerId: string, state: RelayState) => void): () => void
    /** After access was granted: a channel that stopped on "access removed" tries again. */
    restart(computerId: string): Promise<void>
    /** Closes the channel (the Mac was removed from the list). */
    forget(computerId: string): Promise<void>
    /** Asks for this device to join a Mac; resolves once the six digits can be shown to its owner. */
    requestAccess(computerId: string, signKey: string): Promise<CloudResult<AccessTicket>>
    accessStatus(requestId: string): Promise<CloudResult<AccessProgress>>
    cancelAccess(computerId: string, requestId: string | null): Promise<void>
  }
  /** Invite links: the one that arrived, and the token kept (encrypted, per computer) so a reconnect can prove again. */
  invites: {
    takePending(): Promise<InviteLink | null>
    keep(computerId: string, inviteId: string, token: string): Promise<void>
    /** Whether a token is kept for it; the token itself stays out of the window unless `kept` is asked for it. */
    has(computerId: string): Promise<boolean>
    kept(computerId: string): Promise<{ inviteId: string; token: string } | null>
    forget(computerId: string): Promise<void>
  }
  updates: {
    state(): Promise<UpdateState>
    onChange(cb: (s: UpdateState) => void): () => void
    check(): void
    /** Downloads the available release if needed, then restarts into it. */
    install(): void
    setAutoCheck(on: boolean): void
    setAutoDownload(on: boolean): void
  }
  files: {
    begin(file: SharedFile): Promise<string | null>
    write(id: string, offset: number, data: Uint8Array): Promise<void>
    finish(id: string): Promise<void>
    cancel(id: string): Promise<void>
  }
  app: {
    /** Error tracking: an uncaught error in this window (shared/error-report.ts). */
    reportError(report: ErrorReport): void
    openExternal(url: string): void
    /** A long-form memory note in the user's editor; only paths inside a memory folder open. */
    openMemoryFile(path: string): void
    copy(text: string): void
    /** Files picked in the system dialog: name and bytes. */
    pickFiles(): Promise<{ name: string; data: Uint8Array }[]>
    /** The system folder dialog (this computer only); null when cancelled. */
    pickFolder(defaultPath?: string | null): Promise<string | null>
    readClipboardFiles(): Promise<{ name: string; data: Uint8Array }[]>
    setTraySummary(summary: TraySummary): void
    /** Brings the chat window to the front (an incoming call). */
    raiseWindow(): void
    onCommand(cb: (command: WindowCommand) => void): () => void
    resetAllData(): Promise<void>
    setLaunchAtLogin(on: boolean): Promise<boolean>
    showInDock(): Promise<boolean>
    setShowInDock(on: boolean): Promise<boolean>
    /** Sets nativeTheme.themeSource for every window and remembers it for the next launch. */
    setAppearance(appearance: 'system' | 'light' | 'dark'): void
    launchAtLogin(): Promise<boolean>
    openSettings(url: string): void
    /** Opens (or focuses) the Settings window, on `page` when given (a SettingsPage id). */
    openSettingsWindow(page?: string): void
    /** Pops the native Profile menu at the pointer; resolves with the chosen item, null when dismissed. */
    profileMenu(menu: ProfileMenuSpec): Promise<ProfileMenuChoice | null>
    /**
     * Opens `url` in the system browser and resolves with the URL it returns to on `scheme`
     * (`kith://…`), like ASWebAuthenticationSession. Rejects when cancelled or timed out.
     */
    authenticate(url: string, scheme: string): Promise<string>
    /** macOS: registers or removes the Kith Screen agent (SMAppService). */
    screenAgentState(): Promise<ScreenAgentState>
    setScreenAgent(on: boolean): Promise<{ needsApproval: boolean }>
    syncScreenAgent(enabled: boolean): Promise<{ needsApproval: boolean }>
  }
}
