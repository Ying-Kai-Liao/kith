// Wire types for kith-serve (see host/src/api/). Field names match the
// host's camelCase JSON. Timestamps are epoch milliseconds.

import type { GrantedAgent } from './mac-role.ts'
import type { SecretAccount, SecretRequest } from './secret-card.ts'
import { normalizeFolder, type PlaceFolder } from './place-folders.ts'

export type { PlaceFolder } from './place-folders.ts'

export type BotStatus = 'idle' | 'working' | 'needsInput' | 'error'

export interface Bot {
  id: string
  /** `agent`, or `group`: several bots and the user in one chat. */
  kind: string
  members: string[]
  name: string
  description: string
  avatarColor: string
  avatarShape: string
  /** `auto` | `crew:<name>` | `spare:<name>` | `shape`; hosts before this field send nothing, which is `auto`. */
  avatarSource: string
  backend: string
  command?: string | null
  cwd: string
  /** The bot's default Kith project (`BotConfig.project`); places with no pick of their own follow it. */
  project: string | null
  /** Where the bot's own chat works; null for a group, absent from a host without place folders. */
  folder: PlaceFolder | null
  /** Host-allocated personal workspace rather than a user-selected project. */
  managedWorkspace: boolean
  /** `ask` or `auto`. */
  permission: string
  model?: string | null
  /** The model's own settings (effort, fast…): agent option id → value. */
  modelOptions: Record<string, string>
  /** The agent's Fast mode (Claude Code, Codex): faster replies for more usage. */
  fastMode: boolean
  /** What the agent says about Fast mode: `unknown` (no session yet), `offered`, `unsupported`,
   *  or `refused` (it turned it off; `fastModeIssue` says why). */
  fastModeSupport: string
  fastModeIssue: string | null
  pinned: boolean
  /** Where the user dragged it, lowest first (after pinned bots); ties sort by recent activity. */
  position: number
  hidden: boolean
  notify?: boolean | null
  connectors: string[]
  skills: string[]
  /** The bot can see and operate the computer's desktop. */
  computer: boolean
  /** Uses the memory all the user's bots share ("About you"). */
  sharedMemory: boolean
  /** Keeps the harness's own memory (Claude Code's…) on beside Kith's; null where it has no switch. */
  harnessMemory: boolean | null
  /** Moves whenever memory changes, so an open memory screen fetches it again. */
  memoryRev: number
  /** The Kith agent this bot runs (`kith <agent>`); the host sends it only on Kith bots. */
  hqAgent?: string | null
  autoName: boolean
  /** Each burst of the user's chat is a task with its own thread and session; false keeps one conversation. */
  tasks: boolean
  createdAt: number
  rev: number
  status: BotStatus | string
  activity: string
  startedAt?: number | null
  workingChat?: string | null
  workingThread?: string | null
  /** Every place it works in at once (its chat, threads, groups); null from a host that runs one place at a time. */
  workingLanes?: WorkingLane[] | null
  unread: number
  lastMessage?: string | null
  lastAt: number
  /** A bot guest's conversations with this agent (a per-person copy of the chat); absent on every other row and from an older host. */
  botGuest?: BotGuestRef
}

/** Who the outside bot is behind a bot-guest chat (host `bot_json`). */
export interface BotGuestRef {
  botId: string
  name: string
  provider: string
  /** The invited person who owns it; null when the computer's owner does. */
  ownerPerson: string | null
  ownerName: string | null
}

function normalizeBotGuest(raw: unknown): BotGuestRef | undefined {
  if (!raw || typeof raw !== 'object') return undefined
  const r = raw as Record<string, unknown>
  const s = (v: unknown) => (typeof v === 'string' && v ? v : null)
  const botId = s(r.botId)
  if (!botId) return undefined
  return { botId, name: s(r.name) ?? 'Bot', provider: s(r.provider) ?? '', ownerPerson: s(r.ownerPerson), ownerName: s(r.ownerName) }
}

/** A place a bot works in: a chat (its own or a group) and, in a thread, the thread's root. */
export interface WorkingLane {
  chat: string
  thread?: string | null
}

/** Lenient: a host that is a little newer or older than the app still yields a usable bot. */
export function normalizeBot(raw: Partial<Bot> & { id: string }): Bot {
  const createdAt = raw.createdAt ?? 0
  return {
    id: raw.id,
    kind: raw.kind ?? 'agent',
    members: raw.members ?? [],
    name: raw.name ?? 'Bot',
    description: raw.description ?? '',
    avatarColor: raw.avatarColor ?? '',
    avatarShape: raw.avatarShape ?? 'blob',
    avatarSource: raw.avatarSource ?? 'auto',
    backend: raw.backend ?? 'custom',
    command: raw.command ?? null,
    cwd: raw.cwd ?? '',
    project: raw.project || null,
    folder: raw.kind === 'group' ? null : normalizeFolder(raw.folder),
    managedWorkspace: raw.managedWorkspace ?? false,
    permission: raw.permission ?? 'auto',
    model: raw.model ?? null,
    modelOptions: raw.modelOptions ?? {},
    fastMode: raw.fastMode ?? false,
    fastModeSupport: raw.fastModeSupport ?? 'unknown',
    fastModeIssue: raw.fastModeIssue ?? null,
    pinned: raw.pinned ?? false,
    position: raw.position ?? 0,
    hidden: raw.hidden ?? false,
    notify: raw.notify ?? null,
    connectors: raw.connectors ?? [],
    skills: raw.skills ?? [],
    computer: raw.computer ?? false,
    sharedMemory: raw.sharedMemory ?? raw.managedWorkspace ?? false,
    harnessMemory: raw.harnessMemory ?? null,
    memoryRev: raw.memoryRev ?? 0,
    hqAgent: raw.hqAgent || null,
    autoName: raw.autoName ?? false,
    // A host sends `tasks` only when it's off; one without tasks never marks a message as one.
    tasks: raw.kind !== 'group' && raw.tasks !== false,
    createdAt,
    rev: raw.rev ?? 0,
    status: raw.status ?? 'idle',
    activity: raw.activity ?? '',
    startedAt: raw.startedAt ?? null,
    workingChat: raw.workingChat ?? null,
    workingThread: raw.workingThread ?? null,
    workingLanes: raw.workingLanes ?? null,
    unread: raw.unread ?? 0,
    lastMessage: raw.lastMessage ?? null,
    lastAt: raw.lastAt ?? createdAt,
    ...(raw.botGuest !== undefined && normalizeBotGuest(raw.botGuest) ? { botGuest: normalizeBotGuest(raw.botGuest)! } : {}),
  }
}

export const isGroup = (b: Bot) => b.kind === 'group'
export const isWorking = (b: Bot) => b.status === 'working' || b.status === 'needsInput'
export const needsInput = (b: Bot) => b.status === 'needsInput'
/** Working right now in this chat's main conversation (`thread == null`) or in that thread. */
export const isWorkingIn = (b: Bot, chat: string, thread: string | null | undefined) =>
  b.workingLanes
    ? b.workingLanes.some((l) => l.chat === chat && (l.thread ?? null) === (thread ?? null))
    : isWorking(b) && (b.workingChat ?? b.id) === chat && (b.workingThread ?? null) === (thread ?? null)
export const folderName = (b: Bot) =>
  b.managedWorkspace ? 'Personal workspace' : (b.cwd.split('/').filter(Boolean).pop() ?? b.cwd)

export interface FileDiff {
  path: string
  added: number
  removed: number
  isNew: boolean
  startLine: number
  patch: string
}

export interface PermissionOption {
  optionId: string
  name: string
  /** allow_once | allow_always | reject_once | reject_always */
  kind: string
}

export interface PlanItem {
  content: string
  status: string
  priority?: string
}

export interface ThreadSummary {
  count: number
  lastAt: number
  /** Who replied, first reply first: bot ids, and `user`. */
  authors: string[]
  unread?: number
  /** Where the conversation started: absent = in the app; `terminal`; later `bot`, `discord`, `telegram`, `slack`. Open string (see `thread-writer.ts`). */
  origin?: string
  originLabel?: string
  /** A bot-guest thread: whose bot started it. */
  originBot?: { provider?: string; owner?: string; name?: string }
  /** Who may write now: absent/null = the app (or nobody), `terminal`, later `bot`. Open string; any value but `app` holds the thread. */
  liveWriter?: string | null
  /** The writer is mid-turn. */
  liveBusy?: boolean
  terminal?: { sessionId?: string; cwd?: string }
  /** A task's status as the host says it (`tasks.rs`): working, needsYou, stopped, failed, done. Absent from an older host and from a terminal's thread. */
  status?: string
}

export interface Attachment {
  id: string
  name: string
  size: number
}

export interface SharedFile extends Attachment { sha256: string }

export const isImageName = (name: string) =>
  ['png', 'jpg', 'jpeg', 'heic', 'gif', 'webp', 'tiff', 'bmp'].includes(name.split('.').pop()?.toLowerCase() ?? '')

export interface ConnectionRequest {
  [key: string]: unknown
}

export interface BotMessage {
  sourceBotId: string
  targetBotId: string
  text: string
  reply?: string
  detail?: string
}

/** "Earlier context was summarized": the harness's summary, when it gave one. */
export interface Compaction {
  summary?: string | null
  /** The host started it ahead of time; otherwise the harness did when its context filled. */
  byHost?: boolean
}

/** What a new conversation carries when the bot's own chat starts over; editable until the next message sends it. */
export interface HandoffCard {
  /** draft · sent · kept */
  status: string
  /** cold · away · full */
  reason?: string
  text?: string
}

/** An old run holding a session's writer claim: terminal | oneshot | bot | app (open string); `since` is RFC 3339. */
export interface HeldRun {
  session: string
  kind: string
  pid: number
  since: string
}

export interface EntryData {
  /** Structured conversation data added to the existing team-tool notice. */
  botMessage?: BotMessage
  /** notice: the agent summarized its context; kept out of the chat. */
  compaction?: Compaction
  /** notice: the bot's chat starts over with this handoff (see `HandoffNotice`). */
  handoff?: HandoffCard
  heading?: string
  delegationId?: string
  sourceBotId?: string
  targetBotId?: string
  connectionRequest?: ConnectionRequest
  /** A memory line an older host put in the chat. Memory lives in the bot's memory settings, so it isn't shown. */
  memory?: unknown
  memoryFile?: unknown
  routineId?: string
  runId?: string
  text?: string
  final?: boolean
  /** agent: the backup chain entry that answered (`codex`, `claude:work`); null or absent when the agent's own account did. */
  answeredBy?: string | null
  /** agent: how full the session is, in percent; the host sets it on a turn's last entry only when it should show. */
  contextPercent?: number
  /** user: queued | sent | cancelled | failed · tool: pending | in_progress | completed | failed · permission: pending | answered | cancelled | expired */
  status?: string
  clientNonce?: string
  title?: string
  toolKind?: string
  output?: string
  diffs?: FileDiff[]
  locations?: { path: string; line?: number }[]
  options?: PermissionOption[]
  selected?: string
  /** permission, answered: the host's words for what became of the answer when it isn't the option's own (an "always" kith-auth couldn't keep stayed a once grant). Shown in place of the option's outcome. */
  outcome?: string
  command?: string
  detail?: string
  cwd?: string
  /** permission, in an invited person's run: who answers it (a guest's go to the owner), whose run it is and which agent. */
  approver?: 'owner' | 'member'
  person?: string
  personName?: string
  agent?: string
  /** permission, answered: an old run still holds the session this approval resumes in (null/absent otherwise; an older host never sends it). */
  held?: HeldRun | null
  /** permission: an Access or Key card for a service key (shared/secret-card.ts). Never a value. */
  card?: 'account' | 'key'
  secret?: SecretRequest
  accounts?: SecretAccount[]
  entries?: PlanItem[]
  /** notice: info | error | divider */
  style?: string
  /** notice (divider): startedInTerminal | openedInTerminal | tookOver | terminalClosed; open string. */
  writerChange?: string
  /** notice: when the writer changed, in ms. */
  at?: number
  author?: string
  /** user, in a bot-guest chat: the outside bot (and the person speaking for it) that wrote this message. */
  sender?: { botId?: string; name?: string; speaker?: string }
  thread?: ThreadSummary
  /** user, in the main chat: the first message of a task; the task goes on in its thread (see `tasks`). */
  task?: boolean
  reactions?: string[]
  attachments?: Attachment[]
  files?: SharedFile[]
  callSeconds?: number
  /** notice: a voice call that began and may still be going; `callSeconds` replaces it at the end. */
  callLive?: boolean
}

export interface Entry {
  id: string
  seq: number
  botId: string
  threadId?: string | null
  rev: number
  /** user | agent | thought | tool | plan | permission | notice */
  kind: string
  turn: number
  data: EntryData
  createdAt: number
  updatedAt: number
}

/** Chat-visible (Grok Bot model): user messages, final replies, approval cards, notices. */
export function isChat(e: Entry): boolean {
  switch (e.kind) {
    case 'user':
    case 'permission':
      return true
    case 'notice':
      // Only a fresh conversation gets a line: memory lives in the bot's memory settings, a summary keeps the same
      // conversation going, and a kept handoff changed nothing.
      return !e.data.memory && !e.data.memoryFile && !e.data.compaction && e.data.handoff?.status !== 'kept'
    case 'agent':
      return e.data.final === true
    default:
      return false
  }
}

export interface UsageWindow {
  id: string
  label: string
  percent: number
  resetsAt?: number | null
  resetsText?: string | null
}

export interface UsageProvider {
  id: string
  name: string
  windows: UsageWindow[]
  source: string
  updatedAt: number
}

export interface Usage {
  providers: UsageProvider[]
}

export interface Backend {
  id: string
  name: string
  available: boolean
  command: string
  installHint: string
  registry?: string
  description?: string
  installed?: boolean
  curated?: boolean
  signedIn?: boolean
  canInstall?: boolean
  /** Only for an agent that runs on Node.js, on a computer without its own: Kith's copy is missing or added. */
  node?: 'missing' | 'added'
}

export interface ScreenDisplay {
  id: number
  name: string
  width: number
  height: number
  main: boolean
}

export interface ScreenState {
  /** The helper's display name in operating-system permission settings. */
  permissionApp?: string
  enabled: boolean
  connected: boolean
  platform: string
  capture: boolean
  input: boolean
  displays: ScreenDisplay[]
  userControl: boolean
  agentBot?: string | null
  /** Bots with computer use can act here (Windows: always; elsewhere Remote screen is on). */
  computerUse: boolean
  viewers: number
}

export function normalizeScreen(raw: Partial<ScreenState> | undefined | null): ScreenState | null {
  if (!raw) return null
  return {
    enabled: raw.enabled ?? false,
    connected: raw.connected ?? false,
    platform: raw.platform ?? '',
    permissionApp: raw.permissionApp,
    capture: raw.capture ?? false,
    input: raw.input ?? false,
    displays: raw.displays ?? [],
    userControl: raw.userControl ?? false,
    agentBot: raw.agentBot ?? null,
    computerUse: raw.computerUse ?? false,
    viewers: raw.viewers ?? 0,
  }
}

export interface Hello {
  hostId: string
  name: string
  version: string
  os: string
  device?: string
  home?: string
  backends: Backend[]
  rev: number
  screen?: ScreenState
  urls?: string[]
  computerId?: string
  signKey?: string
  boxKey?: string
  /** Absent from a host before 0.1.0 (docs/reference/compatibility.md). */
  protocol?: number
  minProtocol?: number
  cloud?: string | null
  /** Product analytics on this computer: on unless its owner turned it off. */
  analytics?: boolean
  /** For an invited person: the agents the owner gave them. Absent from an owner's hello and from a host before roles. */
  grantedAgents?: GrantedAgent[] | null
}

/** Events only the desktop app knows about; the host records everything else from API calls. */
export type AnalyticsEvent = 'app_opened' | 'onboarding_completed' | 'signed_in' | 'signed_out'

export interface PairingInfo {
  pairingUrl: string
  urls: string[]
  expiresAt?: number
}

export interface DirItem {
  name: string
  path: string
  isGit: boolean
  /** The Kith project registered at this folder. */
  project?: string | null
}

export interface DirListing {
  path: string
  parent?: string | null
  isGit: boolean
  project?: string | null
  dirs: DirItem[]
  /** Where browsing can start: the home folder and each mounted drive. */
  roots?: { name: string; path: string }[]
}

/** A pending request as the computer shows it (loopback `accessRequests`). */
export interface AccessRequest {
  requestId: string
  deviceKey: string
  deviceName: string
  platform?: string | null
  email?: string | null
  /** The SAS; nil until the phone revealed its nonce (Approve stays disabled). */
  code?: string | null
  createdAt?: number | null
  expiresAt?: number | null
}

/** A device the host authorized (loopback `devices`). */
export interface AuthorizedDevice {
  key: string
  name: string
  platform?: string | null
  /** `local` (QR) or `account`. */
  source: string
  scopes?: string[]
  leaseUntil?: number | null
  createdAt?: number | null
  lastSeenAt?: number | null
  connected?: boolean | null
}

export interface CloudOwner {
  userId: string
  email?: string | null
}

/** `code`: compare the 6-digit code at the computer; `auto`: let account devices in. */
export type AccountApproval = 'code' | 'auto'

export interface CloudStatus {
  enabled: boolean
  url?: string | null
  registered?: boolean | null
  connected?: boolean | null
  /** The owner this computer pinned; the cloud alone can't change it. */
  owner?: CloudOwner | null
  /** The cloud names another owner (`owner` null: none); held until confirmed on this computer. */
  ownerChange?: { owner?: CloudOwner | null } | null
  lastError?: string | null
  approval?: AccountApproval | null
}

export interface ClaimSignature {
  computerId: string
  signKey: string
  boxKey: string
  name: string
  platform: string
  device?: string | null
  version: string
  sig: string
}

/** A computer this device talks to. Holds no secrets: only pinned public keys and addresses. */
export interface Computer {
  id: string
  name: string
  signKey: string
  boxKey?: string | null
  urls: string[]
  cloud?: string | null
  device?: string | null
  color?: string | null
}

/** Create / update payload for a bot. */
export interface BotDraft {
  id?: string
  name: string
  description: string
  avatarColor: string
  avatarShape: string
  avatarSource?: string
  backend: string
  command?: string | null
  cwd: string
  /** Default Kith project; null or '' clears it. */
  project?: string | null
  permission: string
  model?: string | null
  modelOptions?: Record<string, string>
  fastMode?: boolean
  pinned?: boolean
  hidden?: boolean
  notify?: boolean | null
  connectors?: string[]
  skills?: string[]
  computer?: boolean
  sharedMemory?: boolean
  harnessMemory?: boolean
}

export function draftOf(bot: Bot): BotDraft {
  return {
    id: bot.id,
    name: bot.name,
    description: bot.description,
    avatarColor: bot.avatarColor,
    avatarShape: bot.avatarShape,
    avatarSource: bot.avatarSource,
    backend: bot.backend,
    command: bot.command ?? null,
    cwd: bot.managedWorkspace ? '' : bot.cwd,
    project: bot.project,
    permission: bot.permission,
    model: bot.model ?? null,
    modelOptions: bot.modelOptions,
    fastMode: bot.fastMode,
    pinned: bot.pinned,
    hidden: bot.hidden,
    notify: bot.notify ?? null,
    connectors: bot.connectors,
    skills: bot.skills,
    computer: bot.computer,
  }
}

export interface GroupDraft {
  id?: string
  kind: 'group'
  name: string
  description: string
  members: string[]
  pinned?: boolean
}
