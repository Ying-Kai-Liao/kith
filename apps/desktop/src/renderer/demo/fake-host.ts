import type { CallError } from '@shared/ipc'
import { isChat, normalizeBot, type Bot, type BotDraft, type Entry, type GroupDraft } from '@shared/models'
import { isAccountName } from '@shared/secret-card'
import * as story from './story'

// An in-memory kith-serve for the website demo: the methods the renderer calls, with the
// host's JSON shapes (host/src/api/), and the events stream (hello, then catch-up in rev
// order, then live). Every mutation stamps a global rev, like the real host.

const TYPING_MS = 1500

export const DEMO_UNAVAILABLE = 'Not available in the web demo. Install Kith to use this.'

export class FakeHost {
  readonly hostId = 'demo-host'
  private rev = 0
  private seq = 0
  private bots = new Map<string, Bot>()
  private entries: Entry[] = []
  private listeners = new Set<(line: string) => void>()
  private turns = new Map<string, number>()
  private nextReply = new Map<string, number>()
  /** Already chosen, so the demo never asks for consent. */
  private analytics = true
  /** The people the owner invited and their open invites (host methods `people`, `invites`). */
  private people: Record<string, unknown>[] = [
    { personId: 'mia', name: 'Mia', agents: { pacer: 'member', relay: 'guest' }, projects: [], joined: true, openInvites: 0, devices: [], autoBots: false },
    { personId: 'leo', name: 'Leo', agents: { pacer: 'guest' }, projects: [], joined: false, openInvites: 0, devices: [], autoBots: false },
  ]
  private invites: Record<string, unknown>[] = []
  /** The grants ledger as `accessLedger` returns it (fake people, folders and sites). */
  private grants: { key: string; kind: string; agent: string; person: string; name: string; target: string; since: number | null }[] = [
    { key: 'g1', kind: 'read', agent: 'pacer', person: 'mia', name: 'Mia', target: '~/docs', since: Date.now() - 12 * 86_400_000 },
    { key: 'g2', kind: 'domain', agent: 'pacer', person: 'mia', name: 'Mia', target: 'pypi.org', since: null },
  ]
  /** Saved keys as `listKeys` returns them: names and who holds them, never a value (fake accounts only). */
  private keys: { profile: string; service: string; account: string; owner: string; vars: string[]; agents: string[]; last_used: string | null }[] = [
    { profile: 'github/work', service: 'github', account: 'work', owner: 'kai', vars: ['GITHUB_TOKEN'], agents: ['scout', 'reviewer'], last_used: new Date(Date.now() - 3_600_000).toISOString() },
    { profile: 'github/personal', service: 'github', account: 'personal', owner: 'kai', vars: ['GITHUB_TOKEN'], agents: ['pacer'], last_used: new Date(Date.now() - 3 * 86_400_000).toISOString() },
    { profile: 'notion/library', service: 'notion', account: 'library', owner: 'kai', vars: ['NOTION_TOKEN'], agents: [], last_used: null },
  ]
  /** Bots from outside, one in each state (host methods `bots`, `decideBot`, ...). */
  private botGuests: Record<string, unknown>[] = ['invited', 'joining', 'pending', 'active', 'denied', 'revoked'].map((state, i) => ({
    botId: `bg_${state}`, name: ['Rex', 'Pip', 'Ada', 'Max', 'Zed', 'Old'][i], provider: i % 2 ? 'other' : 'grok', state,
    ownerPerson: state === 'pending' || state === 'denied' ? 'mia' : null, ownerName: state === 'pending' || state === 'denied' ? 'Mia' : null,
    agents: state === 'pending' ? ['pacer', 'relay', 'notes', 'house'] : ['pacer'], limits: { ratePerHour: 60, concurrent: 2 }, connected: state === 'active',
    createdAt: Date.now() - 2 * 86_400_000, joinedAt: null, decidedAt: null, decidedBy: state === 'active' ? 'kai' : null, lastSeenAt: null,
    revokedAt: state === 'revoked' ? Date.now() - 3_600_000 : null, revokedWhy: null, personId: `bot-${i}`, conversations: [],
  }))

  /** Room conversations by `chat`+lane, as the host's `conversation` event has them. */
  private conversations = new Map<string, Record<string, unknown>>()

  constructor(
    readonly version: string,
    now = Date.now(),
  ) {
    for (const b of story.bots) {
      this.bots.set(b.id, normalizeBot({ ...b, cwd: b.kind === 'group' ? '' : story.cwd, createdAt: now - 86_400_000, permission: b.permission ?? 'auto' }))
    }
    for (const s of story.entries) this.add({ botId: s.botId, threadId: s.threadId ?? null, kind: s.kind, data: s.data }, s.id, now - s.at * 60_000, false)
    // `?secrets` puts an Access card and a Key card in the demo (screenshots of the key cards).
    if (typeof location !== 'undefined' && /[?&]secrets\b/.test(location.search)) {
      for (const s of story.secretEntries) this.add({ botId: s.botId, threadId: null, kind: s.kind, data: s.data }, s.id, now - s.at * 60_000, false)
    }
    // `?room=live|livebot|idle|spent|quiet|stopped|late|bot` puts a room run in the Ship room (screenshots of the agent conversations).
    const room = typeof location !== 'undefined' ? /[?&]room=(\w+)/.exec(location.search)?.[1] : undefined
    if (room) this.seedRoom(room, now)
    for (const b of this.bots.values()) {
      this.refreshPreview(b)
      b.rev = ++this.rev
    }
  }

  private conversationOf(chat: string, lane: string | null) {
    return this.conversations.get(`${chat}\u0000${lane ?? ''}`) ?? null
  }

  private setConversation(c: Record<string, unknown>, emit = true) {
    this.conversations.set(`${c.chat}\u0000${(c.lane as string | null) ?? ''}`, c)
    if (emit) this.emit({ type: 'conversation', ...c })
  }

  /** One run of the Ship room in a state; the entries it left are in the story's room already. */
  private seedRoom(kind: string, now: number) {
    const min = (n: number) => now - n * 60_000
    const bot = kind === 'livebot' || kind === 'bot'
    const grok = { botId: 'bg_grok', name: 'Grok', provider: 'grok' }
    const ship = this.bots.get('ship')
    this.botGuests.push({
      botId: 'bg_grok', name: 'Grok', provider: 'grok', state: 'active', ownerPerson: null, ownerName: null, agents: [], limits: { ratePerHour: 60, concurrent: 2 },
      connected: true, createdAt: min(3000), joinedAt: min(3000), decidedAt: min(3000), decidedBy: 'kai', lastSeenAt: min(2), revokedAt: null, revokedWhy: null, personId: 'bot-grok', conversations: [],
    })
    if (bot && ship) ship.members = [...ship.members, 'bg_grok']
    const add = (id: string, kind: Entry['kind'], at: number, data: Entry['data']) => this.add({ botId: 'ship', threadId: null, kind, data }, id, at, false)
    if (bot) add('room-join', 'notice', min(14), { notice: 'botJoined', bot: grok, style: 'info', text: 'Grok joined · sees this room from now on' })
    add('room-u1', 'user', min(12), { text: bot ? '@Grok what do people say about the name "Your crew"?' : 'Fine, static on mobile. Now the copy block.', status: 'sent' })
    if (bot) {
      add('room-g1', 'user', min(11), { text: 'Mostly warm. "Crew" reads as people you trust; two replies called it a gaming word. Nobody confused it with a product.', status: 'sent', sender: grok, conversation: 'rc1' })
      add('room-g2', 'user', min(5), { text: 'One more thing: nobody asked, but the domain is free.', status: 'sent', sender: grok, unprompted: true })
    }
    add('room-a1', 'agent', min(10), { text: bot ? 'Good. Keeping it. @Grok any name clash in the AI space?' : 'The copy block is two lines: what it is, and who it is for.', final: true, author: 'scout', conversation: 'rc1' })
    if (kind === 'late' || kind === 'stopped') {
      add('room-a2', 'agent', min(9), { text: 'Yes, but still: the first frame only, no motion under 600 px.', final: true, author: 'reviewer', conversation: 'rc1' })
    }
    if (kind === 'late') {
      add('room-late', 'agent', min(6), { text: 'Then mobile is a static PNG, 20 minutes more. I will draft the copy block next.', final: true, author: 'scout', conversation: 'rc1', late: true, lateWhy: 'stopped' })
      add('room-late-bot', 'user', min(4), { text: 'Also: it clashes with a small startup, but not in this space.', status: 'sent', sender: grok, conversation: 'rc1', late: true, lateWhy: 'ended' })
    }
    const base = { conversation: 'rc1', chat: 'ship', lane: null, budget: 10, opening: 'room-u1', waitUntil: null, current: null }
    const states: Record<string, Record<string, unknown>> = {
      live: { ...base, left: 6, state: 'open', endedWhy: null, current: { id: 'scout', kind: 'agent', name: 'scout' } },
      livebot: { ...base, left: 7, state: 'open', endedWhy: null, current: { id: 'bg_grok', kind: 'bot', name: 'Grok' }, waitUntil: now + 5 * 60_000 },
      bot: { ...base, left: 7, state: 'open', endedWhy: null, current: { id: 'bg_grok', kind: 'bot', name: 'Grok' }, waitUntil: now + 5 * 60_000 },
      idle: { ...base, left: 8, state: 'open', endedWhy: null },
      spent: { ...base, left: 0, state: 'ended', endedWhy: 'spent' },
      quiet: { ...base, left: 4, state: 'ended', endedWhy: 'quiet' },
      stopped: { ...base, left: 6, state: 'stopped', endedWhy: 'stopped' },
      late: { ...base, left: 6, state: 'stopped', endedWhy: 'stopped' },
    }
    const c = states[kind]
    if (c) this.setConversation(c, false)
  }

  // MARK: events

  /** `GET /events?since=`: hello, everything after `since` in rev order, then live events. */
  subscribe(since: number, onData: (line: string) => void) {
    const catchUp: { rev: number; line: object }[] = [
      ...[...this.bots.values()].filter((b) => b.rev > since).map((b) => ({ rev: b.rev, line: { type: 'bot', rev: b.rev, bot: this.botJSON(b) } })),
      ...this.entries.filter((e) => e.rev > since).map((e) => ({ rev: e.rev, line: { type: 'entry', rev: e.rev, entry: e } })),
    ].sort((a, b) => a.rev - b.rev)
    const hello = { type: 'hello', hostId: this.hostId, computerId: story.COMPUTER_ID, rev: this.rev, usage: story.usage(Date.now()), screen: null }
    let open = true
    const listener = (line: string) => open && onData(line)
    setTimeout(() => {
      if (!open) return
      for (const item of [{ line: hello }, ...catchUp]) onData(JSON.stringify(item.line))
      this.listeners.add(listener)
    }, 0)
    return () => {
      open = false
      this.listeners.delete(listener)
    }
  }

  private emit(event: object) {
    const line = JSON.stringify(event)
    for (const l of this.listeners) l(line)
  }

  private emitBot(bot: Bot) {
    bot.rev = ++this.rev
    this.emit({ type: 'bot', rev: bot.rev, bot: this.botJSON(bot) })
    // A group shows its busy member's status: it changes with them.
    if (bot.kind !== 'group') for (const g of this.bots.values()) if (g.kind === 'group' && g.members.includes(bot.id)) this.emitBot(g)
  }

  private emitEntry(e: Entry) {
    e.rev = ++this.rev
    e.updatedAt = Date.now()
    this.emit({ type: 'entry', rev: e.rev, entry: e })
  }

  /** The host's `bot_json`: a group is busy while one of its members works in it. */
  private botJSON(b: Bot): Bot {
    if (b.kind !== 'group') return b
    const busy = b.members.map((m) => this.bots.get(m)).find((m) => m && m.status !== 'idle' && m.workingChat === b.id)
    return busy
      ? { ...b, status: busy.status, activity: busy.activity, startedAt: busy.startedAt, workingChat: b.id, workingThread: busy.workingThread }
      : b
  }

  // MARK: entries

  private add(e: Pick<Entry, 'botId' | 'threadId' | 'kind' | 'data'>, id: string = crypto.randomUUID(), at = Date.now(), emit = true) {
    const entry: Entry = { ...e, id, seq: ++this.seq, rev: ++this.rev, turn: 1, createdAt: at, updatedAt: at }
    this.entries.push(entry)
    if (entry.threadId) this.summarize(entry.botId, entry.threadId, emit)
    if (emit) {
      this.emit({ type: 'entry', rev: entry.rev, entry })
      const bot = this.bots.get(entry.botId)
      if (bot && !entry.threadId && isChat(entry)) {
        this.refreshPreview(bot)
        if (entry.kind !== 'user') bot.unread++
        this.emitBot(bot)
      }
    }
    return entry
  }

  /** `data.thread` on the root: reply count, last reply, who replied. */
  private summarize(botId: string, root: string, emit: boolean) {
    const rootEntry = this.entries.find((e) => e.id === root)
    if (!rootEntry) return
    const replies = this.entries.filter((e) => e.botId === botId && e.threadId === root && isChat(e))
    const authors: string[] = []
    for (const r of replies) {
      const a = r.kind === 'user' ? 'user' : (r.data.author ?? botId)
      if (!authors.includes(a)) authors.push(a)
    }
    rootEntry.data = { ...rootEntry.data, thread: { ...rootEntry.data.thread, count: replies.length, lastAt: replies.at(-1)?.createdAt ?? 0, authors, unread: 0 } }
    if (emit) this.emitEntry(rootEntry)
  }

  private refreshPreview(b: Bot) {
    const last = this.entries.filter((e) => e.botId === b.id && !e.threadId && (e.kind === 'user' || (e.kind === 'agent' && e.data.final))).at(-1)
    if (!last) return
    const text = last.data.text ?? ''
    b.lastAt = last.createdAt
    b.lastMessage = last.kind === 'user' ? `You: ${text}` : b.kind === 'group' ? `${this.bots.get(last.data.author ?? '')?.name ?? 'A bot'}: ${text}` : text
  }

  // MARK: turns

  private setStatus(botId: string, status: string, activity = '', chat: string | null = null, thread: string | null = null) {
    const bot = this.bots.get(botId)
    if (!bot) return
    Object.assign(bot, { status, activity, startedAt: status === 'idle' ? null : Date.now(), workingChat: chat, workingThread: thread })
    this.emitBot(bot)
  }

  private pending(botId: string) {
    return this.entries.find((e) => e.kind === 'permission' && e.data.author === botId && e.data.status === 'pending') ?? null
  }

  private settle(botId: string) {
    const waiting = this.pending(botId)
    if (waiting) this.setStatus(botId, 'needsInput', `Needs approval: ${waiting.data.title ?? ''}`, waiting.botId)
    else this.setStatus(botId, 'idle')
  }

  /** One bot answers in a chat or thread after a short "Typing…". */
  private async answer(responder: string, chat: string, thread: string | null, text: string, conversation?: string) {
    const turn = (this.turns.get(responder) ?? 0) + 1
    this.turns.set(responder, turn)
    this.setStatus(responder, 'working', 'Typing…', chat, thread)
    await sleep(TYPING_MS + Math.random() * 600)
    if (this.turns.get(responder) !== turn) return
    this.add({ botId: chat, threadId: thread, kind: 'agent', data: { text, final: true, author: responder, ...(conversation ? { conversation } : {}) } })
    this.settle(responder)
  }

  private replyText(botId: string) {
    if (this.pending(botId)) return story.waitingReply
    const pool = story.replies[botId] ?? ['Got it. I\'ll take a look and report back.']
    const i = this.nextReply.get(botId) ?? 0
    this.nextReply.set(botId, i + 1)
    return pool[i % pool.length]!
  }

  /** A message to a group with turns: a run that charges one turn per reply and ends when they are spent. */
  private async runRoom(chat: Bot, thread: string | null, opening: Entry, budget: number) {
    const old = this.conversationOf(chat.id, thread)
    if (old?.state === 'open') this.setConversation({ ...old, state: 'ended', endedWhy: 'newer', current: null, waitUntil: null })
    const id = crypto.randomUUID()
    const run = { conversation: id, chat: chat.id, lane: thread, budget, left: budget, state: 'open', endedWhy: null, current: null, waitUntil: null, opening: opening.id }
    this.setConversation(run)
    const members = chat.members.filter((m) => this.bots.has(m))
    for (let left = budget; left > 0; ) {
      for (const m of members.slice(0, 2)) {
        if (left <= 0) break
        const now = this.conversationOf(chat.id, thread)
        if (now?.conversation !== id || now.state !== 'open') return
        this.setConversation({ ...run, left, current: { id: m, kind: 'agent', name: this.bots.get(m)!.name } })
        await this.answer(m, chat.id, thread, this.replyText(m), id)
        const after = this.conversationOf(chat.id, thread)
        if (after?.conversation !== id || after.state !== 'open') return
        left--
      }
    }
    this.setConversation({ ...run, left: 0, state: 'ended', endedWhy: 'spent' })
  }

  private async converse(chat: Bot, thread: string | null, text: string) {
    let responders = [chat.id]
    if (chat.kind === 'group') {
      const members = chat.members.filter((m) => this.bots.has(m))
      const mentioned = members.filter((m) => text.toLowerCase().includes(`@${this.bots.get(m)!.name.toLowerCase()}`))
      const rootAuthor = thread ? this.entries.find((e) => e.id === thread)?.data.author : undefined
      // A room turn: who was asked, the thread's bot, or the first two members.
      responders = mentioned.length ? mentioned : rootAuthor && members.includes(rootAuthor) ? [rootAuthor] : members.slice(0, 2)
    }
    for (const r of responders) await this.answer(r, chat.id, thread, this.replyText(r))
  }

  // MARK: API

  async call(method: string, b: Record<string, unknown>): Promise<unknown> {
    const str = (k: string) => (typeof b[k] === 'string' ? (b[k] as string) : '')
    switch (method) {
      case 'hello':
        return {
          hostId: this.hostId, computerId: story.COMPUTER_ID, signKey: '', boxKey: '', cloud: null, name: story.HOST_NAME,
          version: this.version, protocol: 1, minProtocol: 1, os: 'macos', device: 'MacBookPro', home: '/Users/demo', rev: this.rev, analytics: this.analytics,
          urls: [], backends: [
            { id: 'claude', name: 'Claude Code', available: true, installed: true, signedIn: true, command: 'claude', installHint: '' },
            { id: 'codex', name: 'Codex', available: true, installed: true, signedIn: true, command: 'codex', installHint: '' },
          ],
        }
      case 'accessRequests':
        return { requests: [] }
      case 'cloudStatus':
        return { enabled: false }
      case 'usage':
        return story.usage(Date.now())
      case 'history':
        return { entries: [], conversation: this.conversationOf(str('botId'), null) }
      case 'thread':
        return { entries: this.entries.filter((e) => e.botId === str('botId') && e.threadId === str('rootId')), conversation: this.conversationOf(str('botId'), str('rootId') || null) }
      case 'send': {
        const chat = this.bots.get(str('botId'))
        if (!chat) throw fail('That bot was deleted.')
        const thread = str('threadId') || null
        const entry = this.add({ botId: chat.id, threadId: thread, kind: 'user', data: { text: str('text'), status: 'sent', clientNonce: str('clientNonce') } })
        if (chat.kind === 'group' && typeof b.budget === 'number') void this.runRoom(chat, thread, entry, b.budget)
        else void this.converse(chat, thread, str('text'))
        return { entry }
      }
      case 'markRead': {
        const bot = this.bots.get(str('botId'))
        if (bot && bot.unread > 0) {
          bot.unread = 0
          this.emitBot(bot)
        }
        return {}
      }
      case 'provideSecret':
        return this.provideSecret(str('entryId'), str('account'), (b.values ?? {}) as Record<string, unknown>)
      case 'listKeys':
        return { keys: this.keys }
      case 'accessLedger':
        return { grants: this.grants.filter((g) => !b.agent || g.agent === b.agent) }
      case 'removeGrant':
        this.grants = this.grants.filter((g) => g.key !== str('key'))
        return { ok: true }
      case 'agentPeople':
        return { agent: str('agent'), month: new Date().toISOString().slice(0, 7), people: { owner: 4.2, mia: 20.1 }, budgets: { mia: 20 } }
      case 'revokeKey': {
        const key = this.keys.find((k) => k.profile === str('profile') && k.service === str('service'))
        if (key) key.agents = key.agents.filter((a) => a !== str('agent'))
        return { ok: true }
      }
      case 'respondPermission':
        return this.respond(str('entryId'), str('optionId') || null)
      case 'react': {
        const e = this.entries.find((x) => x.id === str('entryId'))
        if (!e) throw fail('That message is gone.')
        const reactions = new Set(e.data.reactions ?? [])
        if (!reactions.delete(str('emoji'))) reactions.add(str('emoji'))
        e.data = { ...e.data, reactions: [...reactions] }
        this.emitEntry(e)
        return { entry: e }
      }
      case 'stop': {
        const id = str('botId')
        const run = this.conversationOf(id, str('threadId') || null)
        if (run?.state === 'open') this.setConversation({ ...run, state: 'stopped', endedWhy: 'stopped', current: null, waitUntil: null })
        for (const m of this.bots.get(id)?.members ?? []) this.turns.set(m, (this.turns.get(m) ?? 0) + 1)
        this.turns.set(id, (this.turns.get(id) ?? 0) + 1)
        this.settle(id)
        return {}
      }
      case 'newSession':
        return {}
      case 'createBot':
      case 'updateBot':
        return { bot: this.saveBot(b as unknown as BotDraft | GroupDraft) }
      case 'reorderBots': {
        ;(b.ids as string[]).forEach((id, position) => {
          const bot = this.bots.get(id)
          if (bot) this.emitBot(Object.assign(bot, { position }))
        })
        return {}
      }
      case 'deleteBot': {
        const id = str('botId')
        this.bots.delete(id)
        this.emit({ type: 'bot', rev: ++this.rev, bot: { id, rev: this.rev, deleted: true } })
        return {}
      }
      case 'refreshBackends':
        return { backends: ((await this.call('hello', {})) as { backends: unknown }).backends }
      case 'listDirs':
        return { path: '/Users/demo/code', parent: '/Users/demo', isGit: false, dirs: [{ name: 'pace', path: story.cwd, isGit: true }] }
      case 'memory':
        return {
          shared: true, yours: { entries: [], used: 0, cap: 400 }, bot: { entries: [], used: 0, cap: 1200 },
          tidyUps: [], projectMemory: null, memoryRev: 0,
        }
      case 'routines':
        return { routines: [], runs: [] }
      case 'skills':
      case 'connectors':
      case 'marketSkills':
      case 'marketConnectors':
      case 'credentialLogins':
      case 'composioToolkits':
        return { items: [] }
      case 'devices':
        return { devices: [] }
      case 'voiceStatus':
        return { providers: [] }
      case 'voiceLocalStatus':
      case 'voiceLocalPrepare':
        return { available: true, model: 'phonon-31m-en', state: 'ready', voices: ['alba', 'ben'], defaultVoice: 'alba' }
      case 'voiceLocalSpeak':
        return { audio: SILENT_WAV, format: 'wav', sampleRate: 24000 }
      case 'composioStatus':
        return { configured: false, keyUrl: '' }
      case 'credentialStatus':
        return { provider: '', onePasswordConnected: false }
      case 'setAnalytics':
        this.analytics = b.enabled === true
        return { enabled: this.analytics }
      case 'track':
        return {}
      case 'people':
        return { people: this.people }
      case 'invites':
        return { invites: this.invites }
      case 'createInvite': {
        const agents = (b.agents ?? {}) as Record<string, string>
        const existing = this.people.find((p) => p.personId === b.personId)
        const personId = existing ? String(existing.personId) : str('name').toLowerCase().replace(/\W+/g, '-')
        if (!existing) this.people.push({ personId, name: str('name'), agents: {}, projects: [], joined: false, openInvites: 0, devices: [], autoBots: false })
        // Like the host, the invite grants the membership when it is made.
        const holder = this.people.find((p) => p.personId === personId)
        if (holder) holder.agents = { ...(holder.agents as object), ...agents }
        const inviteId = `inv-${this.invites.length + 1}`
        const expiresAt = Date.now() + 86_400_000
        this.invites.push({ inviteId, personId, agents, createdAt: Date.now(), expiresAt, usedAt: null, revokedAt: null })
        return { inviteId, link: `https://kith.app/join#${inviteId}-demo`, expiresAt, personId }
      }
      case 'revokeInvite':
        for (const i of this.invites) if (i.inviteId === b.inviteId) i.revokedAt = Date.now()
        return {}
      case 'setPersonAccess': {
        const person = this.people.find((p) => p.personId === b.personId)
        if (!person) throw fail(`\`${str('personId')}\` isn't someone you invited`)
        if (b.agents) person.agents = b.agents
        return { person }
      }
      case 'bots':
        return { bots: this.botGuests }
      case 'myBots':
        return { bots: [] }
      case 'createBotInvite': {
        const botId = `bg_${this.botGuests.length + 1}`
        const link = `https://kith.app/b/demo/${botId}#t=demo`
        if (!b.botId) this.botGuests.unshift({ botId, name: str('name'), provider: str('provider'), state: 'invited', ownerPerson: null, ownerName: null, agents: b.agents, limits: { ratePerHour: 60, concurrent: 2 }, connected: false, createdAt: Date.now(), joinedAt: null, decidedAt: null, decidedBy: null, lastSeenAt: null, revokedAt: null, revokedWhy: null, personId: botId, conversations: [] })
        return { botId: str('botId') || botId, inviteId: `bi_${botId}`, expiresAt: Date.now() + 86_400_000, link, kithLink: `kith://bot?i=${botId}`, share: `Install the Kith client, then join my Kith computer.\n\n  kith join '${link}'\n` }
      }
      case 'decideBot': {
        const bot = this.botGuests.find((x) => x.botId === b.botId)
        if (bot && bot.state === 'pending') Object.assign(bot, b.decision === 'allow' ? { state: 'active', decidedBy: 'kai' } : { state: 'denied' })
        return { bot }
      }
      case 'revokeBot': {
        const bot = this.botGuests.find((x) => x.botId === b.botId)
        if (bot) Object.assign(bot, { state: 'revoked', revokedAt: Date.now() })
        return { revoked: true }
      }
      case 'setAutoAllowBots': {
        const person = this.people.find((p) => p.personId === b.personId)
        if (person) person.autoBots = b.on === true
        return { person }
      }
      default:
        throw fail(DEMO_UNAVAILABLE)
    }
  }

  /** The Access and Key cards: the host's answers to `respondPermission` and `provideSecret`, with its error texts. */
  private answerSecret(e: Entry, optionId: string | null) {
    const secret = e.data.secret!
    if (optionId === 'another') {
      // The same entry becomes a Key card in place.
      e.data = { ...e.data, card: 'key', accounts: undefined }
    } else if (optionId?.startsWith('profile:')) {
      e.data = { ...e.data, status: 'answered', secret: { ...secret, profile: optionId.slice(8), state: 'saved' } }
    } else {
      e.data = { ...e.data, status: 'answered', secret: { ...secret, state: 'denied' } }
    }
    this.emitEntry(e)
    return {}
  }

  private provideSecret(entryId: string, account: string, values: Record<string, unknown>) {
    const e = this.entries.find((x) => x.id === entryId)
    const secret = e?.data.secret
    if (!e || !secret || e.data.card !== 'key' || secret.state !== 'pending') throw fail('That request is closed')
    const name = secret.replaces ? (secret.account ?? '') : account
    if (!isAccountName(name)) throw fail("That account name can't be used")
    if (secret.vars.some((v) => typeof values[v] !== 'string' || values[v] === '')) throw fail('Enter every field of the key')
    if (secret.vars.some((v) => (values[v] as string).length > 1536)) throw fail('That key is too long')
    const profile = `${secret.service}/${name}`
    // Values are dropped here: only names are kept, as the host keeps only names outside the Keychain.
    const author = e.data.author ?? e.botId
    this.keys = [...this.keys.filter((k) => k.profile !== profile), { profile, service: secret.service, account: name, owner: 'kai', vars: secret.vars, agents: [author], last_used: null }]
    e.data = { ...e.data, status: 'answered', secret: { ...secret, profile, account: name, state: 'saved' } }
    this.emitEntry(e)
    return { saved: { service: secret.service, vars: secret.vars, profile }, resumed: true }
  }

  private async respond(entryId: string, optionId: string | null) {
    const e = this.entries.find((x) => x.id === entryId)
    if (!e || e.data.status !== 'pending') return {}
    if (e.data.card && e.data.secret) return this.answerSecret(e, optionId)
    const option = e.data.options?.find((o) => o.optionId === optionId)
    const allowed = !!option && option.kind.startsWith('allow')
    e.data = { ...e.data, status: optionId ? 'answered' : 'cancelled', selected: optionId ?? undefined }
    this.emitEntry(e)
    const author = e.data.author ?? e.botId
    void (async () => {
      this.setStatus(author, 'working', allowed ? 'Editing src/pace.js' : 'Typing…', e.botId)
      await sleep(TYPING_MS)
      this.add({ botId: e.botId, threadId: e.threadId ?? null, kind: 'agent', data: { text: allowed ? story.approvedReply : story.deniedReply, final: true, author } })
      this.settle(author)
    })()
    return {}
  }

  private saveBot(draft: BotDraft | GroupDraft) {
    const existing = draft.id ? this.bots.get(draft.id) : undefined
    const bot = normalizeBot({
      ...(existing ?? { createdAt: Date.now(), cwd: 'cwd' in draft ? draft.cwd || story.cwd : '' }),
      ...(draft as Partial<Bot>),
      id: existing?.id ?? crypto.randomUUID(),
    })
    if (existing) Object.assign(existing, bot)
    const saved = existing ?? bot
    this.bots.set(saved.id, saved)
    this.emitBot(saved)
    return this.botJSON(saved)
  }
}

function fail(message: string): CallError {
  return { status: 400, message }
}

/** 0.1 s of silence as a 24 kHz mono 16-bit WAV, base64: what a demo call "speaks". */
const SILENT_WAV = (() => {
  const data = 4800
  const bytes = new Uint8Array(44 + data)
  const view = new DataView(bytes.buffer)
  const text = (at: number, s: string) => [...s].forEach((c, i) => (bytes[at + i] = c.charCodeAt(0)))
  text(0, 'RIFF')
  view.setUint32(4, 36 + data, true)
  text(8, 'WAVEfmt ')
  view.setUint32(16, 16, true)
  view.setUint16(20, 1, true)
  view.setUint16(22, 1, true)
  view.setUint32(24, 24000, true)
  view.setUint32(28, 48000, true)
  view.setUint16(32, 2, true)
  view.setUint16(34, 16, true)
  text(36, 'data')
  view.setUint32(40, data, true)
  return btoa(String.fromCharCode(...bytes))
})()

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
