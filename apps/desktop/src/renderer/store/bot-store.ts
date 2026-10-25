import {
  isWorking,
  normalizeBot,
  needsInput,
  type AnalyticsEvent,
  type Bot,
  type BotDraft,
  type DirListing,
  type Entry,
  type GroupDraft,
  type Hello,
  type ScreenState,
  type Usage,
} from '@shared/models'
import { normalizeFolder, type PlaceFolder, type PlaceSelection, type StartingFolder } from '@shared/place-folders'
import { partialUpdate } from '@shared/optimistic'
import { expireCalls, type IncomingCall } from '@shared/incoming-call'
import { failureOf, OWN_TURN, type HeldPhase } from '@shared/held-run'
import { FileDownloads } from './file-downloads'
import { BotSync, sleep } from './bot-sync'

export { builtInBackendName, type Connection } from './bot-mirror'

/** A file picked in the composer, sent with the next message. */
export interface OutgoingFile {
  id: string
  name: string
  data: Uint8Array
}

/** Anything larger is refused by the computer. */
export const MAX_FILE_SIZE = 100 * 1024 * 1024

export const QUICK_REACTIONS = ['👍', '❤️', '😂', '🎉', '👀', '✅']

/**
 * One computer's mirror (bots, transcripts), kept by one events stream (catch-up since
 * `rev`, then live). Port of kit's `BotStore`.
 */
export class BotStore extends BotSync {
  readonly fileDownloads = new FileDownloads(() => this.changed())

  override retire() { this.fileDownloads.retire(); super.retire() }

  private outgoingFiles = new Map<string, OutgoingFile[]>()
  /** The folder picked for a message that has not reached the host yet, so Resend keeps it. */
  private outgoingPlaces = new Map<string, StartingFolder>()

  /** Where each thread works, as the host last said (`thread`, `setPlaceFolder`); keyed `bot:root`. */
  private threadFolders = new Map<string, PlaceFolder | null>()
  threadFolder(botId: string, root: string) { return this.threadFolders.get(`${botId}:${root}`) ?? null }

  /** `newTask`: in a bot's chat, start a task rather than go on with the current one. */
  send(text: string, botId: string, thread: string | null = null, files: OutgoingFile[] = [], nonce: string = crypto.randomUUID(), pasted: string[] = [], newTask = false, place: StartingFolder | null = null, via: 'call' | null = null) {
    const trimmed = text.trim()
    if ((!trimmed && !files.length) || this.retired) return
    const data: Entry['data'] = { text: trimmed, status: 'sending', clientNonce: nonce }
    if (place) this.outgoingPlaces.set(nonce, place)
    if (files.length) {
      data.attachments = files.map((f) => ({ id: f.id, name: f.name, size: f.data.length }))
      this.outgoingFiles.set(nonce, files)
    }
    this.upsert({
      id: `local-${nonce}`, seq: Number.MAX_SAFE_INTEGER, botId, threadId: thread, rev: 0, kind: 'user',
      turn: 0, data, createdAt: Date.now(), updatedAt: 0,
    })
    this.changed()
    void (async () => {
      try {
        // Safe to repeat: the computer skips a nonce it already has.
        const e = await this.withLink(async (client) => {
          let ids: string[] | null = null
          const pending = this.outgoingFiles.get(nonce)
          if (pending) {
            ids = []
            for (const file of pending) {
              const id = crypto.randomUUID()
              await client.upload(botId, id, file.name, file.data)
              cacheAttachment(id, file.data)
              ids.push(id)
            }
          }
          return client.send(botId, trimmed, nonce, thread, ids, pasted, newTask, place, via)
        }, true)
        this.outgoingFiles.delete(nonce)
        this.outgoingPlaces.delete(nonce)
        this.upsert(e)
        this.changed()
      } catch (error) {
        if (this.outgoingFiles.has(nonce)) this.lastError = `Couldn't send the files: ${error instanceof Error ? error.message : error}`
        this.markLocal(nonce, botId, 'failed')
      }
    })()
  }

  retry(entry: Entry) {
    const files = (entry.data.clientNonce && this.outgoingFiles.get(entry.data.clientNonce)) || []
    const text = entry.data.text ?? ''
    if (!text && !files.length) return
    this.entries.set(entry.botId, this.allEntries(entry.botId).filter((e) => e.id !== entry.id))
    const place = (entry.data.clientNonce && this.outgoingPlaces.get(entry.data.clientNonce)) || null
    this.send(text, entry.botId, entry.threadId ?? null, files, entry.data.clientNonce ?? crypto.randomUUID(), [], false, place)
  }

  discard(entry: Entry) {
    if (entry.data.clientNonce) {
      this.outgoingFiles.delete(entry.data.clientNonce)
      this.outgoingPlaces.delete(entry.data.clientNonce)
    }
    this.entries.set(entry.botId, this.allEntries(entry.botId).filter((e) => e.id !== entry.id))
    this.changed()
    this.scheduleSave()
  }

  /** Takes back a message that still waits for its turn; the host marks it cancelled. */
  unsend(entry: Entry) {
    this.perform(async (c) => {
      const res = await c.call<{ entry: Entry }>('unsend', { botId: entry.botId, entryId: entry.id })
      this.upsert(res.entry)
      this.changed()
    })
  }

  private markLocal(nonce: string, botId: string, status: string) {
    const list = this.entries.get(botId)
    const i = list?.findIndex((e) => e.id === `local-${nonce}`) ?? -1
    if (!list || i < 0) return
    const next = [...list]
    next[i] = { ...next[i]!, data: { ...next[i]!.data, status } }
    this.entries.set(botId, next)
    this.changed()
    this.scheduleSave()
  }

  /** A sent file's bytes: cached after the first fetch (sent files never change). */
  async attachmentData(fileId: string, botId: string): Promise<Uint8Array | null> {
    const cached = attachmentCache.get(fileId)
    if (cached) return cached
    const client = this.live
    if (!client) return null
    try {
      const data = await client.readUpload(botId, fileId)
      cacheAttachment(fileId, data)
      return data
    } catch {
      return null
    }
  }

  /** Stops the bot in its chat, or in the thread on `thread`; its other places keep working. */
  stop(botId: string, thread: string | null = null) {
    this.perform((c) => c.call('stop', thread ? { botId, threadId: thread } : { botId }), true)
  }

  newSession(botId: string) {
    this.perform((c) => c.call('newSession', { botId }))
  }

  /** The call the person just accepted, until its chat opens and starts the voice call (see `ThreadView`). */
  callToStart: { botId: string; callId: string } | null = null

  /** Accepts an incoming call; false when another device or the timeout got there first. */
  async acceptCall(call: IncomingCall) {
    try {
      await (await this.ready()).callAccept(call.id)
    } catch {
      this.incomingCalls = this.incomingCalls.filter((c) => c.id !== call.id)
      this.changed()
      return false
    }
    this.incomingCalls = this.incomingCalls.filter((c) => c.id !== call.id)
    this.callToStart = { botId: call.botId, callId: call.id }
    this.changed()
    return true
  }

  /** Drops the calls whose ring is over (the host's "missed" may not reach a device that lost the link). */
  expireCalls() {
    const left = expireCalls(this.incomingCalls)
    if (left === this.incomingCalls) return
    this.incomingCalls = left
    this.changed()
  }

  declineCall(call: IncomingCall) {
    this.incomingCalls = this.incomingCalls.filter((c) => c.id !== call.id)
    this.changed()
    this.perform((c) => c.callDecline(call.id))
  }

  /** The accepted call for `botId`, once: the chat that starts it takes it. */
  takeCallStart(botId: string) {
    const start = this.callToStart
    if (start?.botId !== botId) return null
    this.callToStart = null
    return start.callId
  }

  greetCall(callId: string) {
    this.perform((c) => c.callGreet(callId))
  }

  /**
   * The call connected: its chat line goes in now, where the call began. Resolves to the line's id, or null on a
   * computer older than this (or a failure); `finishCall` then writes the old end-of-call line.
   */
  startCall(botId: string): Promise<string | null> {
    return this.withLink((c) => c.call<{ entry: { id: string } }>('startCall', { botId }))
      .then((r) => r.entry.id)
      .catch(() => null)
  }

  /** The call ended: its length goes on the line `startCall` made. `started` is null when the call never connected. */
  async finishCall(botId: string, started: Promise<string | null> | null, seconds: number) {
    const entryId = await started
    if (started && !entryId) {
      this.logCall(botId, seconds)
      return
    }
    // A computer older than endCall just doesn't learn the call ended; there is no line to write.
    await this.withLink((c) => c.call('endCall', { botId, entryId, seconds }), true).catch(() => {})
  }

  logCall(botId: string, seconds: number) {
    this.perform((c) => c.call('logCall', { botId, seconds }))
  }

  screenTakeover(on: boolean) {
    this.perform(async (c) => {
      this.screen = await c.call<ScreenState>('screenTakeover', { on })
      this.changed()
    }, true)
  }

  /** Only the computer itself may turn remote screen on. */
  async setScreenEnabled(on: boolean) {
    this.screen = await (await this.ready()).call<ScreenState>('setScreenEnabled', { enabled: on })
    this.changed()
  }

  /** Product analytics on this computer; undefined before hello. */
  get analytics() {
    return this.hello?.analytics
  }

  /** Only the computer itself may change analytics (the host accepts it from loopback). */
  async setAnalytics(enabled: boolean) {
    const res = await (await this.ready()).call<{ enabled: boolean }>('setAnalytics', { enabled })
    if (this.hello) this.hello = { ...this.hello, analytics: res.enabled }
    this.changed()
  }

  /** A one-time code the person sends to their Kith bot as `/link CODE`; asking again gives a fresh one. */
  linkCode() {
    return this.withLink((c) => c.call<LinkCode>('linkCode', {}))
  }

  /** Fire-and-forget; the host holds it until this computer answers and drops it on a no. */
  track(event: AnalyticsEvent) {
    void this.withLink((c) => c.call('track', { event })).catch((error: unknown) => console.warn(`track ${event} failed`, error))
  }

  /** Toggles the user's reaction; shown at once, then replaced by the host's copy. */
  react(entry: Entry, emoji: string) {
    const list = this.entries.get(entry.botId)
    const i = list?.findIndex((e) => e.id === entry.id) ?? -1
    if (!list || i < 0) return
    const reactions = [...(list[i]!.data.reactions ?? [])]
    const at = reactions.indexOf(emoji)
    if (at >= 0) reactions.splice(at, 1)
    else reactions.push(emoji)
    const next = [...list]
    next[i] = { ...next[i]!, data: { ...next[i]!.data, reactions } }
    this.entries.set(entry.botId, next)
    this.changed()
    const previous = list[i]!.data.reactions
    this.perform(async (c) => {
      const res = await c.call<{ entry: Entry }>('react', { entryId: entry.id, emoji })
      this.upsert(res.entry)
      this.changed()
    }, false, () => {
      const now = this.entries.get(entry.botId)
      const at = now?.findIndex((e) => e.id === entry.id) ?? -1
      if (!now || at < 0) return
      const back = [...now]
      back[at] = { ...back[at]!, data: { ...back[at]!.data, reactions: previous } }
      this.entries.set(entry.botId, back)
    })
  }

  respond(entry: Entry, option: string | null) {
    if (this.answering.has(entry.id)) return
    this.answering.set(entry.id, option ?? '')
    this.changed()
    void (async () => {
      try {
        await this.withLink((c) => c.call('respondPermission', { entryId: entry.id, optionId: option }), true)
        // The card's own update follows on the events stream; hold the spinner until then.
        await sleep(2000)
      } catch (error) {
        this.lastError = error instanceof Error ? error.message : String(error)
      }
      this.answering.delete(entry.id)
      this.changed()
    })()
  }

  /**
   * Sends a typed key from a Key card. The values pass through here and are kept nowhere (no store field, no mirror,
   * no log); a failure throws the host's own text for the card to show, and the call is never replayed.
   */
  provideSecret(entry: Entry, account: string, values: Record<string, string>) {
    return this.withLink((c) => c.provideSecret(entry.id, account, values))
  }

  listKeys() {
    return this.withLink((c) => c.listKeys())
  }

  revokeKey(agent: string, service: string, profile: string) {
    return this.withLink((c) => c.revokeKey(agent, service, profile))
  }

  /**
   * The owner's Stop on a card's held row: ends the old run holding the card's session. One at a time per session (a second
   * click, or the same session's other card, does nothing while it runs). The rows close when the host clears `held` on
   * the entries, not here.
   */
  stopHolder(entry: Entry) {
    const held = entry.data.held
    if (!held || this.holderPhases.get(held.session)?.kind === 'stopping') return
    this.holderPhases.set(held.session, { kind: 'stopping' })
    this.changed()
    void (async () => {
      let phase: HeldPhase | null = null
      try {
        const res = await this.withLink((c) => c.stopHolder(held.session), true)
        if (res.ownTurn) phase = OWN_TURN
        else if (res.stopped) this.stoppedRuns.set(entry.id, { kind: held.kind, at: new Date() })
      } catch (error) {
        phase = failureOf(error, this.hostName)
      }
      if (phase) this.holderPhases.set(held.session, phase)
      else this.holderPhases.delete(held.session)
      this.changed()
    })()
  }

  /** Saves the user's version of a handoff that hasn't been sent; whether it did. */
  async editHandoff(entry: Entry, text: string) {
    try {
      await this.withLink((c) => c.call('editHandoff', { entryId: entry.id, text }), true)
      return true
    } catch (error) {
      this.lastError = error instanceof Error ? error.message : String(error)
      this.changed()
      return false
    }
  }

  /** Carries on with the bot's current conversation instead of the handoff. */
  keepConversation(entry: Entry) {
    if (this.answering.has(entry.id)) return
    this.answering.set(entry.id, 'handoff')
    this.changed()
    void (async () => {
      try {
        await this.withLink((c) => c.call('keepConversation', { entryId: entry.id }), true)
      } catch (error) {
        this.lastError = error instanceof Error ? error.message : String(error)
      }
      this.answering.delete(entry.id)
      this.changed()
    })()
  }

  /** The roster's "Mark as read": the chat and all its threads. */
  markAllRead(botId: string) {
    const bot = this.bots.get(botId)
    if (!bot || bot.unread <= 0) return
    this.bots.set(botId, { ...bot, unread: 0 })
    this.changed()
    this.perform((c) => c.call('markRead', { botId, threadId: null, all: true }), true)
  }

  private patchBot(bot: Bot, change: Partial<BotDraft>) {
    const current = this.bots.get(bot.id)
    if (current) this.bots.set(bot.id, { ...current, ...change } as Bot)
    this.changed()
    // Only the changed key goes out (the host merges), so a stale local copy can't revert other edits.
    this.perform((c) => c.call('updateBot', partialUpdate(bot.id, change)), true, () => {
      const now = this.bots.get(bot.id)
      if (!now) return
      const back: Record<string, unknown> = {}
      for (const key of Object.keys(change)) back[key] = (bot as unknown as Record<string, unknown>)[key]
      this.bots.set(bot.id, { ...now, ...back } as Bot)
    })
  }

  setPinned(bot: Bot, pinned: boolean) {
    this.patchBot(bot, { pinned })
  }

  /** Whether the bot may see and use the computer's screen. */
  setComputer(bot: Bot, computer: boolean) {
    this.patchBot(bot, { computer })
  }

  /** Whether the bot uses the memory all bots share. */
  setSharedMemory(bot: Bot, sharedMemory: boolean) {
    this.patchBot(bot, { sharedMemory })
  }

  /** Whether the bot's harness keeps its own memory on beside Kith's. */
  setHarnessMemory(bot: Bot, harnessMemory: boolean) {
    this.patchBot(bot, { harnessMemory })
  }

  setHidden(bot: Bot, hidden: boolean) {
    this.patchBot(bot, { hidden })
  }

  setFastMode(bot: Bot, fastMode: boolean) {
    this.patchBot(bot, { fastMode })
  }

  /** The roster as the user dragged it, top first: shown at once, then saved on the host. */
  reorder(ids: string[]) {
    const before = new Map<string, number>()
    ids.forEach((id, position) => {
      const bot = this.bots.get(id)
      if (!bot) return
      before.set(id, bot.position)
      this.bots.set(id, { ...bot, position })
    })
    this.changed()
    this.perform((c) => c.call('reorderBots', { ids }), true, () => {
      for (const [id, position] of before) {
        const bot = this.bots.get(id)
        if (bot) this.bots.set(id, { ...bot, position })
      }
    })
  }

  delete(bot: Bot) {
    const kept = this.entries.get(bot.id)
    this.composerDrafts.removeBot(bot.id)
    this.bots.delete(bot.id)
    this.entries.delete(bot.id)
    this.onRosterChanged?.()
    this.changed()
    this.perform((c) => c.call('deleteBot', { botId: bot.id }), true, () => {
      this.bots.set(bot.id, bot)
      if (kept) this.entries.set(bot.id, kept)
      this.onRosterChanged?.()
    })
  }

  /** Creates a group chat (or opens the one these bots already share) and selects it. */
  async createGroup(name: string, description: string, members: string[]) {
    const draft: GroupDraft = { kind: 'group', name, description, members }
    const res = await (await this.ready()).call<{ bot: Bot }>('createBot', draft)
    this.bots.set(res.bot.id, res.bot)
    this.selection = res.bot.id
    this.onRosterChanged?.()
    this.changed()
    return res.bot
  }

  async updateGroup(draft: GroupDraft) {
    const res = await (await this.ready()).call<{ bot: Bot }>('updateBot', draft)
    this.bots.set(res.bot.id, res.bot)
    this.changed()
  }

  /** Takes one bot out of a group; an automatic "Alice, Bob" name follows the new roster. */
  removeMember(group: Bot, memberId: string) {
    const members = group.members.filter((m) => m !== memberId)
    const autoName = (ids: string[]) => ids.map((id) => this.bots.get(id)?.name).join(', ')
    const name = group.name === autoName(group.members) ? autoName(members) : group.name
    this.bots.set(group.id, { ...group, name, members })
    this.changed()
    const draft: GroupDraft = { id: group.id, kind: 'group', name, description: group.description, members, pinned: group.pinned }
    this.perform((c) => c.call('updateBot', draft), true, () => {
      if (this.bots.has(group.id)) this.bots.set(group.id, group)
    })
  }

  async loadThread(botId: string, root: string) {
    if (!this.client) return
    try {
      const res = await this.client.thread(botId, root)
      for (const e of res.entries) this.upsert(e)
      this.threadFolders.set(`${botId}:${root}`, normalizeFolder(res.folder))
      this.changed()
    } catch (error) {
      this.reportFailure('load the thread', error)
    }
  }

  private reportFailure(what: string, error: unknown) {
    console.warn(`Couldn't ${what}`, error)
    this.setError(`Couldn't ${what}: ${error instanceof Error ? error.message : String(error)}`)
  }

  /** The history between a bot and one peer. Never upserted: old notices would break `loadOlder`'s paging. */
  async botConversation(botId: string, peerId: string): Promise<Entry[]> {
    return (await this.ready()).botConversation(botId, peerId)
  }

  async save(draft: BotDraft) {
    const client = await this.ready()
    const res = draft.id
      ? await client.call<{ bot: Bot }>('updateBot', { ...draft, model: draft.model ?? null })
      : await client.call<{ bot: Bot }>('createBot', draft)
    this.bots.set(res.bot.id, res.bot)
    this.onRosterChanged?.()
    this.changed()
    return res.bot
  }

  async refreshBackends() {
    if (!this.client || !this.hello) return
    try {
      const res = await this.client.call<{ backends: Hello['backends'] }>('refreshBackends', {}, 60_000)
      this.hello = { ...this.hello, backends: res.backends }
      this.changed()
    } catch {}
  }

  async listDirs(path: string | null) {
    return (await this.ready()).call<DirListing>('listDirs', { path })
  }

  async projects(refresh = false) {
    return (await this.ready()).projects(refresh)
  }

  async recentFolders() {
    return (await this.ready()).recentFolders()
  }

  async registerProject(id: string, path: string) {
    return (await this.ready()).registerProject(id, path)
  }

  /** Points a place (a bot's chat, or the thread on `threadId`) at a folder; the host starts it fresh. */
  async setPlaceFolder(botId: string, selection: PlaceSelection, threadId: string | null = null) {
    const res = await (await this.ready()).setPlaceFolder(botId, selection, { threadId })
    const folder = normalizeFolder(res.folder)
    if (threadId) this.threadFolders.set(`${botId}:${threadId}`, folder)
    else {
      const bot = this.bots.get(botId)
      if (bot) this.bots.set(botId, { ...bot, folder })
    }
    this.changed()
    return folder
  }

  async refreshUsage() {
    if (this.retired || !this.client) return
    try {
      this.usage = await this.client.call<Usage>('usage', { refresh: true })
      this.changed()
    } catch (error) {
      this.reportFailure('refresh usage', error)
    }
  }

  async loadOlder(botId: string) {
    if (!this.client || this.historyComplete.has(botId)) return
    const first = this.chat(botId).find((e) => e.seq > 0 && e.seq !== Number.MAX_SAFE_INTEGER)?.seq ?? Number.MAX_SAFE_INTEGER
    try {
      const older = await this.client.history(botId, first, 100)
      if (older.length < 100) this.historyComplete.add(botId)
      for (const e of older) this.upsert(e)
      this.changed()
    } catch (error) {
      this.reportFailure('load older messages', error)
    }
  }

  /** An invited person's own chat with a granted agent; opens it. */
  async startChat(agent: string) {
    try {
      const { bot } = await this.withLink((c) => c.startChat(agent))
      const next = normalizeBot(bot)
      this.bots.set(next.id, next)
      this.onRosterChanged?.()
      this.setSelection(next.id)
    } catch (error) {
      this.lastError = error instanceof Error ? error.message : String(error)
      this.changed()
    }
  }

  setSelection(id: string | null) {
    this.selection = id
    this.changed()
  }

  consumeRoutineDraft(botId: string) {
    const value = this.routineDrafts.get(botId)
    if (value !== undefined) {
      this.routineDrafts.delete(botId)
      this.changed()
    }
    return value
  }

  setRoutineDraft(botId: string, text: string) {
    this.routineDrafts.set(botId, text)
    this.changed()
  }
}

// ponytail: in-memory attachment cache; a disk cache (main process) if relaunches refetching hurts.
const attachmentCache = new Map<string, Uint8Array>()
function cacheAttachment(id: string, data: Uint8Array) {
  attachmentCache.set(id, data)
}

export { isWorking, needsInput }

/** `linkCode`: the code and when it stops working (epoch milliseconds). */
export interface LinkCode {
  code: string
  expiresAt: number
}
