import { parseCall, type IncomingCall } from '@shared/incoming-call'
import { parseAgentPeople, parseAgentUsage, parseUsageReport, type AgentPeople, type AgentUsage, type AtLimit, type Period, type UsageReport } from '@shared/usage-limits'
import type { CallError } from '@shared/ipc'
import type { RelayCallError } from '@shared/relay'
import { parseBackups, type Backups } from '@shared/backups'
import { parseBotGuest, parseBotGuests, parseBotRequest, parseCreatedBotInvite, type BotGuest, type BotProvider, type CreatedBotInvite } from '@shared/bot-guests'
import { parseKeys, type KeyRow } from '@shared/secret-card'
import type { StopHolderResult } from '@shared/held-run'
import {
  parseDiff, parseHistory, parseMemoryList, parseRead, parseSave, parseTidyUps, parseUndo,
  type HqMemoryDiff, type HqMemoryList, type HqMemoryRead, type HqMemoryVersion, type HqSaveResult, type HqTidyUp, type HqUndoResult,
} from '@shared/hq-memory'
import { isUnsupportedHost, parseKithInfoResult, parseSearchResult, type KithInfoResult, type KithSearchResult } from '@shared/kith-info'
import { isAccessRemoved, parseGranted } from '@shared/mac-role'
import { parseAudit, parseCreatedInvite, parseInvites, parsePeople, type AuditRow, type CreatedInvite, type PendingInvite, type Person, type Role } from '@shared/people'
import { threadWriterCode, threadWriterMessage, type TakeOverResult, type ThreadWriterCode } from '@shared/thread-writer'
import type { PlaceFolder, PlaceSelection, ProjectInfo, RecentFolder, StartingFolder } from '@shared/place-folders'
import { METHODS, receive, type MethodName, type Params, type Received, type Results } from '@shared/host-contract.gen'
import {
  asEntry,
  normalizeBot,
  normalizeScreen,
  type AccessRequest,
  type Bot,
  type CloudStatus,
  type Entry,
  type Hello,
  type ScreenState,
  type Usage,
} from '@shared/models'

/** How a client reaches a host API: loopback (this computer's own host or an SSH tunnel), or the Kith cloud relay. */
export type HostRoute = 'loopback' | 'direct' | 'relay'

export class HostError extends Error {
  constructor(
    readonly status: number,
    message: string,
    /** The relay link's word for why (`accessRemoved`, `leaseExpired`, ...), or `refused` for a definite no from the Mac. */
    readonly code?: string,
  ) {
    super(message)
  }

  /** Access to a relayed Mac is gone for good (not a lease that may be renewed). */
  get isAccessRemoved() {
    return isAccessRemoved(this.status, this.code)
  }

  static unreachable() {
    return new HostError(0, "Can't reach your computer. Is it on and connected to the internet?")
  }

  /** The link failed, not the request: the same call can work once the computer is reachable again. */
  get isTransient() {
    return this.status === 0
  }
}

/** A refusal about who holds a thread: a send to one open in a terminal, or a take-over the terminal's work stops. */
export class ThreadWriterError extends HostError {
  constructor(
    readonly writerCode: ThreadWriterCode,
    from: HostError,
  ) {
    super(from.status, threadWriterMessage(from.message), from.code)
  }
}

/** Turns a host 409 about the thread's writer into a `ThreadWriterError`; anything else passes through. */
function typedWriterError(error: unknown): unknown {
  const code = threadWriterCode(error)
  return code && error instanceof HostError ? new ThreadWriterError(code, error) : error
}

export interface HostTransport {
  call(method: string, body: unknown, timeoutMs: number): Promise<unknown>
  /** Each element is one event's JSON (what SSE puts after `data:`). */
  stream(path: string, onData: (line: string) => void, onEnd: (error: HostError | null) => void): () => void
}

export class LoopbackTransport implements HostTransport {
  constructor(
    readonly baseURL: string,
    private token: string,
  ) {}

  async call(method: string, body: unknown, timeoutMs: number) {
    try {
      return await window.kith.host.call(this.baseURL, this.token, method, body, timeoutMs)
    } catch (e) {
      const error = e as CallError
      throw new HostError(error.status ?? 0, error.status === 401 ? "This device isn't allowed on that computer anymore." : error.message)
    }
  }

  stream(path: string, onData: (line: string) => void, onEnd: (error: HostError | null) => void) {
    return window.kith.host.stream(`${this.baseURL}${path}`, this.token, onData, (error) =>
      onEnd(error ? new HostError(error.status, error.message) : null),
    )
  }
}

/**
 * Another Mac over the Kith cloud relay. The encrypted channel lives in the main process (shared by every window);
 * this only carries calls and the event stream across the bridge, with the same errors as the loopback transport so a
 * dropped link looks the same to the stores (status 0, retry) and a host's own refusal passes through unchanged.
 */
export class RelayTransport implements HostTransport {
  constructor(
    readonly computerId: string,
    /** The Mac's own Ed25519 key (b64url); it must hash to `computerId`. */
    private signKey: string,
  ) {}

  async call(method: string, body: unknown, timeoutMs: number) {
    try {
      return await window.kith.relay.call(this.computerId, this.signKey, method, body, timeoutMs)
    } catch (e) {
      const error = e as Partial<RelayCallError>
      throw new HostError(error.status ?? 0, error.message ?? HostError.unreachable().message, error.code)
    }
  }

  /** `path` is `/events?since=&client=`, like the loopback SSE; the relay carries it as the `events` subscription. */
  stream(path: string, onData: (line: string) => void, onEnd: (error: HostError | null) => void) {
    const url = new URL(path, 'http://relay.invalid')
    const sub = url.pathname.replace(/^\//, '')
    const body = sub === 'events' ? { since: Number(url.searchParams.get('since') ?? 0), client: url.searchParams.get('client') ?? '' } : null
    return window.kith.relay.stream(this.computerId, this.signKey, sub, body, onData, (error) =>
      onEnd(error ? new HostError(error.status, error.message, error.code) : null),
    )
  }
}

export type HostEvent =
  | { type: 'hello'; hostId: string; rev: number; usage: Usage; screen: ScreenState | null }
  | { type: 'bot'; bot: Bot }
  | { type: 'botDeleted'; id: string; rev: number }
  | { type: 'entry'; entry: Entry }
  | { type: 'usage'; usage: Usage }
  | { type: 'screen'; screen: ScreenState }
  | { type: 'accessRequests'; requests: AccessRequest[] }
  | { type: 'cloud'; cloud: CloudStatus }
  | { type: 'resync' }
  /** A bot from outside asks the owner to let it in; only a trigger to read `bots` again. */
  | { type: 'botRequest'; bot: BotGuest }
  /** A bot guest changed state (owner of the computer and the bot's owner get it). */
  | { type: 'botUpdated'; bot: BotGuest }
  /** An agent is calling (ringing), or a call stopped ringing (answered, declined, missed). */
  | { type: 'call'; call: IncomingCall; ringing: boolean }
  /** An event this app version couldn't read (host newer/older than the app). */
  | { type: 'undecodable'; kind: string }

export function parseEvent(line: string): HostEvent | null {
  let raw: Record<string, unknown>
  try {
    raw = JSON.parse(line) as Record<string, unknown>
  } catch {
    return null
  }
  const type = raw.type
  switch (type) {
    case 'hello':
      return {
        type,
        hostId: (raw.hostId as string) ?? '',
        rev: (raw.rev as number) ?? 0,
        usage: (raw.usage as Usage) ?? { providers: [] },
        screen: normalizeScreen(raw.screen as ScreenState | undefined),
      }
    case 'bot': {
      const bot = raw.bot as (Bot & { deleted?: boolean }) | undefined
      if (!bot || typeof bot.id !== 'string') return { type: 'undecodable', kind: 'bot' }
      if (bot.deleted) return { type: 'botDeleted', id: bot.id, rev: bot.rev ?? 0 }
      return { type, bot: normalizeBot(bot) }
    }
    case 'entry': {
      const entry = raw.entry as Entry | undefined
      if (!entry || typeof entry.id !== 'string' || typeof entry.botId !== 'string') return { type: 'undecodable', kind: 'entry' }
      return { type, entry: { ...entry, data: entry.data ?? {} } }
    }
    case 'usage':
      return raw.usage ? { type, usage: raw.usage as Usage } : null
    case 'screen': {
      const screen = normalizeScreen(raw.screen as ScreenState | undefined)
      return screen ? { type, screen } : null
    }
    case 'accessRequests':
      return { type, requests: (raw.requests as AccessRequest[]) ?? [] }
    case 'cloud':
      return raw.cloud ? { type, cloud: raw.cloud as CloudStatus } : null
    case 'resync':
      return { type }
    case 'botRequest': {
      const bot = parseBotRequest(raw.bot)
      return bot ? { type, bot } : null
    }
    case 'botUpdated': {
      const bot = parseBotGuest(raw.bot)
      return bot ? { type, bot } : null
    }
    case 'call': {
      const parsed = parseCall(raw)
      return parsed ? { type, ...parsed } : null
    }
    default:
      return null
  }
}

/** Typed host calls (host/src/api/); anything else goes through `call`. */
export class HostClient {
  readFile(entryId: string, fileId: string, offset: number): Promise<{ data: string; size: number }> {
    return this.call('readFile', { entryId, fileId, offset })
  }

  constructor(readonly transport: HostTransport) {}

  call<T>(method: string, body: unknown = {}, timeoutMs = 20_000): Promise<T> {
    return this.transport.call(method, body, timeoutMs) as Promise<T>
  }

  /**
   * A method of the host RPC contract (serve/contract/host): what it takes is the contract's, and its answer is read as
   * the contract says (`receive`: a word this app doesn't know reads as its enum declares, a left-out list is empty).
   */
  async callHost<M extends MethodName>(method: M, params: Params[M], timeoutMs = 20_000): Promise<Received<Results[M]>> {
    const raw = await this.transport.call(method, params, timeoutMs)
    return receive(METHODS[method].result, raw) as Received<Results[M]>
  }

  /** A method renamed with tidy-ups: a host from before the rename answers "unknown method" to the new name, so try `old` once. */
  async callRenamed<T>(method: string, old: string, body: unknown = {}, timeoutMs = 20_000): Promise<T> {
    try {
      return await this.call<T>(method, body, timeoutMs)
    } catch (error) {
      if (!isUnsupportedHost(error)) throw error
      return this.call<T>(old, body, timeoutMs)
    }
  }

  /** Catch-up since `since`, then live updates. */
  events(since: number, client: string, onEvent: (e: HostEvent) => void, onEnd: (error: HostError | null) => void) {
    const query = new URLSearchParams({ since: String(since), client })
    return this.transport.stream(`/events?${query}`, (line) => {
      const event = parseEvent(line)
      if (event) onEvent(event)
    }, onEnd)
  }

  /** Answering an incoming call (`id` from its event); the first device to answer wins, the others get an error. */
  callAccept(id: string) {
    return this.callHost('callAccept', { id })
  }

  callDecline(id: string) {
    return this.callHost('callDecline', { id })
  }

  /** The accepted call is live: the agent speaks first. */
  callGreet(id: string) {
    return this.callHost('callGreet', { id })
  }

  hello() {
    return this.call<Hello>('hello').then((h) => ({ ...h, backends: h.backends ?? [], screen: normalizeScreen(h.screen) ?? undefined, grantedAgents: parseGranted(h.grantedAgents) }))
  }

  async history(botId: string, beforeSeq: number, limit = 100): Promise<Entry[]> {
    return (await this.callHost('history', { botId, beforeSeq, limit })).entries.map(asEntry)
  }

  /** `folder`: where the thread works; absent from a host without place folders. */
  async thread(botId: string, rootId: string): Promise<{ entries: Entry[]; folder?: unknown }> {
    const got = await this.callHost('thread', { botId, rootId })
    return { ...got, entries: got.entries.map(asEntry) }
  }

  async botConversation(botId: string, peerId: string) {
    return (await this.call<{ entries: Entry[] }>('botConversation', { botId, peerId })).entries
  }

  /** `pasted`: what the user pasted rather than typed, which memory never takes as their own words. */
  async send(botId: string, text: string, clientNonce: string, threadId: string | null, attachments: string[] | null, pasted: string[] = [], newTask = false, place: StartingFolder | null = null, via: 'call' | null = null) {
    try {
      return asEntry((await this.callHost('send', { botId, text, clientNonce, threadId, attachments, ...(pasted.length ? { pasted } : {}), ...(newTask ? { newTask } : {}), ...(place ?? {}), ...(via ? { via } : {}) })).entry)
    } catch (e) {
      throw typedWriterError(e)
    }
  }

  /** Takes a thread over from the terminal that holds it; the host ends an idle terminal. Owner only. Throws `ThreadWriterError` (`terminalBusy`) while it works. */
  async takeOverThread(botId: string, threadId: string): Promise<TakeOverResult> {
    try {
      return await this.call<TakeOverResult>('takeOverThread', { botId, threadId }, 30_000)
    } catch (e) {
      throw typedWriterError(e)
    }
  }

  /** Ends the old run holding a card's session so its approved resume can go on. Owner only (a member gets 403); nothing live is `stopped: null`. */
  stopHolder(session: string) {
    return this.call<StopHolderResult>('stopHolder', { session }, 20_000)
  }

  /** Kith's project registry; without Kith the host answers `projects: []` and an `error` line. */
  projects(refresh = false) {
    return this.call<{ projects: ProjectInfo[]; error?: string }>('projects', refresh ? { refresh: true } : {}, 30_000)
  }

  /** A Kith bot's details from `kith <agent> --info-json`; `refresh` runs kith now instead of taking the host's 30 s copy. A host before this call answers 404. */
  async kithInfo(botId: string, refresh = false): Promise<KithInfoResult> {
    return parseKithInfoResult(await this.callHost('kithInfo', { botId, ...(refresh ? { refresh: true } : {}) }, 60_000))
  }

  /** Writes an hq bot's permission mode through to hq (`kith <agent> set permission`) and returns hq's info read back. */
  async kithSetPermission(botId: string, mode: string): Promise<KithInfoResult> {
    return parseKithInfoResult(await this.call('kithSetPermission', { botId, mode }, 60_000))
  }

  /** Writes an hq bot's call model through to hq (`kith <agent> set call-model`): a model, `own` or `default`. Returns hq's info read back. */
  async kithSetCallModel(botId: string, model: string): Promise<KithInfoResult> {
    return parseKithInfoResult(await this.call('kithSetCallModel', { botId, model }, 60_000))
  }

  /** The backup chain: global order, per-agent orders and each entry's state. A host before this call answers 404. */
  async backups(): Promise<Backups> {
    return parseBackups(await this.call('backups', {}, 90_000))
  }

  /** Replaces a whole order: `agent` null is the global one; `chain` null drops the agent's own order. Answers like `backups`. */
  async backupsSet(agent: string | null, chain: string[] | null): Promise<Backups> {
    return parseBackups(await this.call('backupsSet', { agent, chain }, 90_000))
  }

  async kithMemorySearch(botId: string, query: string, limit = 30): Promise<KithSearchResult> {
    return parseSearchResult(await this.call('kithMemorySearch', { botId, query, limit }, 30_000))
  }

  // An hq agent's memory as files (`kithMemory*`, serve/docs/bot-info.md): `shared` is the user layer, `name` the path inside the layer.
  async hqMemoryList(botId: string): Promise<HqMemoryList> {
    return parseMemoryList(await this.call('kithMemoryList', { botId }, 30_000))
  }

  async hqMemoryRead(botId: string, shared: boolean, name: string): Promise<HqMemoryRead> {
    return parseRead(await this.call('kithMemoryRead', { botId, shared, name }, 30_000))
  }

  async hqMemorySave(botId: string, shared: boolean, name: string, text: string): Promise<HqSaveResult> {
    return parseSave(await this.call('kithMemorySave', { botId, shared, name, text }, 30_000))
  }

  async hqMemoryHistory(botId: string, shared: boolean, name: string, limit = 50): Promise<HqMemoryVersion[]> {
    return parseHistory(await this.call('kithMemoryHistory', { botId, shared, name, limit }, 30_000))
  }

  async hqMemoryDiff(botId: string, shared: boolean, name: string, from: number | null, to: number): Promise<HqMemoryDiff> {
    return parseDiff(await this.call('kithMemoryDiff', { botId, shared, name, ...(from !== null ? { from } : {}), to }, 30_000))
  }

  async hqMemoryRestore(botId: string, shared: boolean, name: string, version: number): Promise<HqSaveResult> {
    return parseSave(await this.call('kithMemoryRestore', { botId, shared, name, version }, 30_000))
  }

  async hqTidyUps(botId: string, limit = 20): Promise<HqTidyUp[]> {
    return parseTidyUps(await this.callRenamed('kithMemoryTidyUps', 'kithMemoryDreams', { botId, limit }, 30_000))
  }

  async hqUndoTidy(botId: string, id: number): Promise<HqUndoResult> {
    return parseUndo(await this.callRenamed('kithMemoryUndoTidy', 'kithMemoryUndoDream', { botId, id }, 30_000))
  }

  // People an owner invited (serve/host/src/api/people.rs). A host before invites answers "unknown method".
  async people(): Promise<Person[]> {
    return parsePeople(await this.call('people'))
  }

  async invites(): Promise<PendingInvite[]> {
    return parseInvites(await this.call('invites'))
  }

  /** `who` is an existing person's id or a new person's name. */
  async createInvite(who: { personId: string } | { name: string }, agents: Record<string, Role>): Promise<CreatedInvite> {
    return parseCreatedInvite(await this.call('createInvite', { ...who, agents }, 60_000))
  }

  revokeInvite(inviteId: string) {
    return this.call('revokeInvite', { inviteId })
  }

  /** `projects` left out keeps the person's projects as they are. */
  setPersonAccess(personId: string, agents: Record<string, Role>, projects?: string[]) {
    return this.call('setPersonAccess', { personId, agents, projects }, 60_000)
  }

  revokePerson(personId: string) {
    return this.call('revokePerson', { personId }, 60_000)
  }

  async audit(since = 0, personId: string | null = null): Promise<AuditRow[]> {
    return parseAudit(await this.call('audit', { since, ...(personId ? { personId } : {}) }))
  }

  // What things cost at API prices and the monthly limits (serve/host/src/limits.rs). Owner only; an older host
  // answers "unknown method" and the Usage page shows the plan meter alone.
  async usageReport(period: Period): Promise<UsageReport> {
    return parseUsageReport(await this.call('usageReport', { period }, 60_000))
  }

  async agentUsage(agent: string, period: Period = 'month'): Promise<AgentUsage> {
    return parseAgentUsage(await this.call('agentUsage', { agent, period }, 60_000))
  }

  /** `limit` dollars a month, null for none; `at` what happens there. Answers the agent's view. */
  async setAgentLimit(agent: string, limit: number | null, at: AtLimit): Promise<AgentUsage> {
    return parseAgentUsage(await this.call('setAgentLimit', { agent, limit, at }, 60_000))
  }

  async agentPeople(agent: string): Promise<AgentPeople> {
    return parseAgentPeople(await this.call('agentPeople', { agent }, 60_000))
  }

  /** A person's budget on one agent, dollars a month; null for none. */
  async setPersonBudget(agent: string, person: string, budget: number | null): Promise<AgentPeople> {
    return parseAgentPeople(await this.call('setPersonBudget', { agent, person, budget }, 60_000))
  }

  /** The first call of an invited device: shows the invite token from the link. Until it works the Mac refuses everything else. */
  proveInvite(inviteId: string, token: string) {
    return this.callHost('proveInvite', { inviteId, token })
  }

  /** An invited person's own chat with a granted agent. */
  startChat(agent: string) {
    return this.call<{ bot: Bot }>('startChat', { agent }, 60_000)
  }

  // Service keys (kernel/PROTOCOL.md, "Keys"). Owner only. No call here ever returns a value.
  /** Saves a typed key to the Keychain. `values` is sent once and kept nowhere: not logged, not stored, not replayed. */
  provideSecret(entryId: string, account: string, values: Record<string, string>): Promise<{ saved: { service: string; vars: string[]; profile: string }; resumed: boolean }> {
    return this.call('provideSecret', { entryId, account, values }, 30_000)
  }

  async listKeys(): Promise<KeyRow[]> {
    return parseKeys(await this.call('listKeys', {}))
  }

  revokeKey(agent: string, service: string, profile: string) {
    return this.call<{ ok: boolean }>('revokeKey', { agent, service, profile })
  }

  // Bots from outside (serve/host/src/api/botguests.rs). A host before them answers "unknown method".
  /** Makes a bot and its invite; `botId` makes a fresh invite for a bot you already have. `share` is secret: show it, never keep it. */
  async createBotInvite(input: { name: string; provider: BotProvider; agents: string[]; botId?: string }): Promise<CreatedBotInvite> {
    return parseCreatedBotInvite(await this.callHost('createBotInvite', input, 60_000))
  }

  /** Every bot, with its conversations. The computer's owner only. */
  async bots(): Promise<BotGuest[]> {
    return parseBotGuests(await this.call('bots'))
  }

  /** The caller's own bots, ended ones too. */
  async myBots(): Promise<BotGuest[]> {
    return parseBotGuests(await this.callHost('myBots', {}))
  }

  /** The computer's owner only. `alwaysForOwner` also lets this person's later bots in without asking. */
  decideBot(botId: string, decision: 'allow' | 'deny', alwaysForOwner = false) {
    return this.call('decideBot', { botId, decision, ...(alwaysForOwner ? { alwaysForOwner: true } : {}) })
  }

  setAutoAllowBots(personId: string, on: boolean) {
    return this.call('setAutoAllowBots', { personId, on })
  }

  revokeBot(botId: string) {
    return this.callHost('revokeBot', { botId })
  }

  async recentFolders() {
    return (await this.call<{ folders: RecentFolder[] }>('recentFolders')).folders
  }

  async registerProject(id: string, path: string) {
    return (await this.call<{ project: ProjectInfo }>('registerProject', { id, path }, 30_000)).project
  }

  /** `threadId` is the thread's root message; `chat` a group's id (a member's lane). */
  setPlaceFolder(botId: string, selection: PlaceSelection, place: { threadId?: string | null; chat?: string } = {}) {
    return this.call<{ folder: PlaceFolder }>('setPlaceFolder', { botId, ...(place.threadId ? { threadId: place.threadId } : {}), ...(place.chat ? { chat: place.chat } : {}), ...selection }, 30_000)
  }

  /** Bytes per `upload` call: base64 of it stays well under the channel's 1 MiB message. */
  static uploadChunk = 384 * 1024

  async upload(botId: string, uploadId: string, name: string, data: Uint8Array) {
    let offset = 0
    do {
      const end = Math.min(offset + HostClient.uploadChunk, data.length)
      await this.callHost('upload', { botId, uploadId, name, data: toBase64(data.subarray(offset, end)), offset, done: end === data.length }, 60_000)
      offset = end
    } while (offset < data.length)
  }

  async readUpload(botId: string, uploadId: string) {
    const parts: Uint8Array[] = []
    let size = 0
    for (;;) {
      const res = await this.callHost('readUpload', { botId, uploadId, offset: size }, 60_000)
      const chunk = fromBase64(res.data)
      if (!chunk.length) break
      parts.push(chunk)
      size += chunk.length
      if (size >= res.size) break
    }
    const out = new Uint8Array(size)
    let at = 0
    for (const p of parts) {
      out.set(p, at)
      at += p.length
    }
    return out
  }
}

export function toBase64(bytes: Uint8Array) {
  let binary = ''
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(binary)
}

export function fromBase64(text: string) {
  const binary = atob(text)
  const out = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i)
  return out
}
