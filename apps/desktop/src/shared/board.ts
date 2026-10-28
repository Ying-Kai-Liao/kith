// The Board beside Crew: what is waiting on you, what is going on, and where things were left, derived from the
// stores the app already has. A port of apps/ios/Kit/Sources/KithUI/Store/BoardModel.swift; nothing here is fetched
// or kept, so a changed store changes the Board. The sidebar only lays these values out.
import { cardHeading, cardQuestion, type BotGuest } from './bot-guests.ts'
import { limitOf, limitQuestion } from './usage-limits.ts'
import type { GuestPrompt } from './mac-role.ts'
import { isWorkingIn, type Bot, type Entry } from './models.ts'
import { isBusyInTerminal } from './thread-writer.ts'
import { isTask, taskLastAt, taskStatus, taskTitle } from './tasks.ts'

/** A room on a computer: what a row opens. */
export interface BoardTarget {
  computerId: string
  botId: string
}

/** What a computer that can't be reached right now is, for the chip on its items. */
export type BoardTrouble = 'unreachable' | 'catchingUp'

/** What one computer gives the Board, as plain values copied from its store. */
export interface BoardComputer {
  id: string
  hostName: string
  trouble?: BoardTrouble | null
  /** The rooms this account sees there: on a joined computer only the agents granted to it. */
  bots: Bot[]
  entries: ReadonlyMap<string, Entry[]>
  /** Guests' requests for this account's answer (owned computers only). */
  guestPrompts?: GuestPrompt[]
  /** Bots from outside waiting for this account's allow (owned computers only). */
  botRequests?: BotGuest[]
}

/** A question or permission waiting on you. */
export interface BoardDecision {
  id: string
  target: BoardTarget
  hostName: string
  /** Who is asking: the bot, or `person · agent` for a guest. */
  botName: string
  /** The task thread it sits in, when it sits in one. */
  thread: string | null
  threadTitle: string | null
  question: string
  detail: string
  at: number
  trouble: BoardTrouble | null
  /** A guest's request: outside any chat of this account's own, so it has no room to open. */
  guest: boolean
  /** An agent's at-limit card (an owner's run waits under it): answered in place on Board, not as a row. */
  limit?: Entry
}

/** A task at work, or one finished today. */
export interface BoardWork {
  id: string
  target: BoardTarget
  hostName: string
  bot: Bot
  title: string
  detail: string
  at: number
  trouble: BoardTrouble | null
}

/** A bot whose chat started over with a handoff, shown as "Where we left off". */
export interface BoardHandoff {
  id: string
  target: BoardTarget
  hostName: string
  botName: string
  bot: Bot
  note: string
  at: number
}

/** Agents working in one project. */
export interface BoardProject {
  id: string
  name: string
  agents: number
  working: number
  /** Task threads in the project's bots, and the questions open in them. */
  threads: number
  decisions: number
  /** The most recently active agent there. */
  target: BoardTarget
}

/** Something the crew is waiting on: messages queued for a bot's turn, or a computer that can't be reached. */
export interface BoardWait {
  id: string
  kind: 'queued' | 'computer'
  queued: number
  target: BoardTarget | null
  hostName: string
  title: string
  detail: string
}

export interface BoardModel {
  needsYou: BoardDecision[]
  working: BoardWork[]
  doneToday: BoardWork[]
  leftOff: BoardHandoff[]
  projects: BoardProject[]
  waits: BoardWait[]
}

/** How many show under Done today and Where we left off. */
export const DONE_LIMIT = 5
export const LEFT_OFF_LIMIT = 3
/** A handoff older than this is no longer where anyone left off. */
export const HANDOFF_AGE_MS = 3 * 86_400_000
/** A handoff's text can be a whole briefing; the row shows its start. */
export const NOTE_LIMIT = 280

const pendingPermission = (e: Entry) => e.kind === 'permission' && e.data.status === 'pending'

/** A card this account may answer, as the host says (`canAnswer`); from an older host, a guest's card in a chat of this account's own is the owner's to answer, not this one's. */
const isMine = (e: Entry) => pendingPermission(e) && (e.data.canAnswer ?? e.data.approver !== 'owner')

/** Questions waiting on you in one chat: the count on a Crew row. */
export const pendingQuestions = (entries: readonly Entry[]) => entries.filter(isMine).length

/** What a permission card wants, as the line a row shows. */
export function decisionQuestion(entry: Entry): string {
  const limit = limitQuestion(entry)
  if (limit) return limit
  if (entry.data.card && entry.data.secret) return `Needs a key for ${entry.data.secret.service}`
  switch (entry.data.toolKind) {
    case 'execute':
      return 'Wants to run a command'
    case 'edit':
    case 'delete':
    case 'move':
      return 'Wants to change files'
    case 'fetch':
      return 'Wants to access the web'
    case 'read':
    case 'search':
      return 'Wants to read files'
    default:
      return 'Wants to use a tool'
  }
}

const byAtDesc = <T extends { at: number; id: string }>(a: T, b: T) => (a.at !== b.at ? b.at - a.at : a.id < b.id ? -1 : 1)

const activity = (bot: Bot) => bot.activity || 'Working…'

export function buildBoard(computers: BoardComputer[], now = Date.now()): BoardModel {
  const dayStart = new Date(now).setHours(0, 0, 0, 0)
  const decisions: BoardDecision[] = []
  const working: BoardWork[] = []
  let done: BoardWork[] = []
  let leftOff: BoardHandoff[] = []
  const waits: BoardWait[] = []
  const projectBots = new Map<string, { bot: Bot; computerId: string; threads: number; decisions: number }[]>()

  for (const computer of computers) {
    const trouble = computer.trouble ?? null
    for (const bot of computer.botRequests ?? []) {
      decisions.push({
        id: `${computer.id}/bot/${bot.botId}`,
        target: { computerId: computer.id, botId: bot.botId },
        hostName: computer.hostName,
        botName: cardHeading(bot),
        thread: null,
        threadTitle: null,
        question: cardQuestion(bot),
        detail: cardQuestion(bot),
        at: bot.createdAt,
        trouble,
        guest: true,
      })
    }
    for (const prompt of computer.guestPrompts ?? []) {
      decisions.push({
        id: `${computer.id}/${prompt.entryId}`,
        target: { computerId: computer.id, botId: prompt.botId },
        hostName: computer.hostName,
        botName: `${prompt.personName} · ${prompt.agent}`,
        thread: null,
        threadTitle: null,
        question: prompt.title,
        detail: prompt.title,
        at: prompt.createdAt,
        trouble,
        guest: true,
      })
    }
    if (trouble) {
      waits.push({
        id: `computer/${computer.id}`,
        kind: 'computer',
        queued: 0,
        target: null,
        hostName: computer.hostName,
        title: computer.hostName,
        detail: trouble === 'unreachable' ? "Can't be reached right now" : 'Catching up',
      })
    }
    for (const bot of computer.bots) {
      const target = { computerId: computer.id, botId: bot.id }
      const entries = computer.entries.get(bot.id) ?? []
      const tasks: Entry[] = []
      const pending: Entry[] = []
      let queued = 0
      let handoff: Entry | null = null
      for (const entry of entries) {
        if (isTask(entry)) tasks.push(entry)
        else if (isMine(entry)) pending.push(entry)
        else if (entry.kind === 'user' && entry.data.status === 'queued') queued++
        else if (entry.kind === 'notice' && entry.data.handoff && entry.data.handoff.status !== 'kept' && (entry.data.handoff.text ?? '').trim() && entry.createdAt > (handoff?.createdAt ?? 0)) handoff = entry
      }
      const titles = new Map(tasks.map((t) => [t.id, taskTitle(t)]))

      for (const entry of pending) {
        decisions.push({
          id: `${computer.id}/${entry.id}`,
          target,
          hostName: computer.hostName,
          botName: bot.name,
          thread: entry.threadId ?? null,
          threadTitle: (entry.threadId && titles.get(entry.threadId)) || null,
          question: decisionQuestion(entry),
          detail: entry.data.title ?? '',
          at: entry.createdAt,
          trouble,
          guest: false,
          ...(limitOf(entry) ? { limit: entry } : {}),
        })
      }

      let taskIsWorking = false
      for (const task of tasks) {
        const lastAt = taskLastAt(task)
        const atWork = isWorkingIn(bot, bot.id, task.id) || isBusyInTerminal(task.data.thread)
        if (!atWork && lastAt < dayStart) continue
        const replies = entries.filter((e) => e.threadId === task.id)
        const status = taskStatus(bot, task, replies)
        if (status === 'working') {
          taskIsWorking = true
          working.push({ id: `${computer.id}/${task.id}`, target, hostName: computer.hostName, bot, title: taskTitle(task), detail: isBusyInTerminal(task.data.thread) ? 'Working in the terminal' : activity(bot), at: lastAt, trouble })
        } else if (status === 'needsYou') {
          // Its question is in Needs you.
          taskIsWorking = true
        } else if (status === 'done') {
          const said = [...replies].reverse().find((e) => e.kind === 'agent' && e.data.final === true)?.data.text
          done.push({ id: `${computer.id}/${task.id}`, target, hostName: computer.hostName, bot, title: taskTitle(task), detail: said?.replace(/\s+/g, ' ').trim() || bot.name, at: lastAt, trouble })
        }
      }
      // A bot at work in its main chat (no task of its own, a group's member, an older host).
      if (!taskIsWorking && isWorkingIn(bot, bot.id, null) && bot.status !== 'needsInput') {
        working.push({ id: `${computer.id}/${bot.id}`, target, hostName: computer.hostName, bot, title: bot.name, detail: activity(bot), at: bot.startedAt ?? bot.lastAt, trouble })
      }

      if (handoff && now - handoff.createdAt < HANDOFF_AGE_MS) {
        const clean = (handoff.data.handoff?.text ?? '').split(/\s+/).filter(Boolean).join(' ')
        leftOff.push({
          id: `${computer.id}/${handoff.id}`,
          target,
          hostName: computer.hostName,
          botName: bot.name,
          bot,
          note: clean.length > NOTE_LIMIT ? `${clean.slice(0, NOTE_LIMIT)}…` : clean,
          at: handoff.createdAt,
        })
      }
      if (queued > 0) {
        waits.push({
          id: `queued/${computer.id}/${bot.id}`,
          kind: 'queued',
          queued,
          target,
          hostName: computer.hostName,
          title: bot.name,
          detail: queued === 1 ? '1 message waiting for its turn' : `${queued} messages waiting for its turn`,
        })
      }
      if (bot.kind !== 'group' && bot.folder?.kind === 'project') {
        const name = bot.folder.project ?? bot.folder.name
        const list = projectBots.get(name) ?? []
        list.push({ bot, computerId: computer.id, threads: tasks.length, decisions: pending.length })
        projectBots.set(name, list)
      }
    }
  }

  done = done.sort(byAtDesc).slice(0, DONE_LIMIT)
  leftOff = leftOff.sort((a, b) => b.at - a.at).slice(0, LEFT_OFF_LIMIT)
  // The computers first, then what waits on a bot.
  waits.sort((a, b) => {
    const l = a.target === null
    const r = b.target === null
    return l !== r ? (l ? -1 : 1) : a.id < b.id ? -1 : 1
  })
  const projects = [...projectBots.entries()].map(([name, members]): BoardProject => {
    const latest = members.reduce((a, b) => (b.bot.lastAt > a.bot.lastAt ? b : a))
    return {
      id: name,
      name,
      agents: members.length,
      working: members.filter((m) => m.bot.status === 'working' || m.bot.status === 'needsInput').length,
      threads: members.reduce((n, m) => n + m.threads, 0),
      decisions: members.reduce((n, m) => n + m.decisions, 0),
      target: { computerId: latest.computerId, botId: latest.bot.id },
    }
  })
  projects.sort((a, b) => (a.working !== b.working ? b.working - a.working : a.name.localeCompare(b.name)))

  return {
    needsYou: decisions.sort((a, b) => (a.at !== b.at ? a.at - b.at : a.id < b.id ? -1 : 1)),
    working: working.sort(byAtDesc),
    doneToday: done,
    leftOff,
    projects,
    waits,
  }
}

/** The total on the Board tab: questions waiting on you, on every computer. */
export const boardWaiting = (board: BoardModel) => board.needsYou.length

/** The room a Return opens on the Board: the first item in the order the Board shows them. */
export function boardTop(board: BoardModel): BoardTarget | null {
  return (
    board.needsYou.find((d) => !d.guest)?.target ??
    board.working[0]?.target ??
    board.doneToday[0]?.target ??
    board.leftOff[0]?.target ??
    board.waits.find((w) => w.target)?.target ??
    board.projects[0]?.target ??
    null
  )
}

/** A Needs you row: the questions of one thread (or one chat) together. */
export interface NeedsYouRow {
  key: string
  target: BoardTarget
  title: string
  /** The oldest question of the group. */
  question: string
  count: number
}

/** Needs you as sidebar rows. Guests' requests and limit cards stay out: they are answered in place. */
export function needsYouRows(decisions: BoardDecision[]): NeedsYouRow[] {
  const rows = new Map<string, NeedsYouRow>()
  for (const d of decisions) {
    if (d.guest || d.limit) continue
    const key = `${d.target.computerId}/${d.target.botId}/${d.thread ?? ''}`
    const row = rows.get(key)
    if (row) row.count++
    else rows.set(key, { key, target: d.target, title: d.threadTitle ?? d.botName, question: d.question, count: 1 })
  }
  return [...rows.values()]
}

/** "4 threads · 2 decisions open", or the agents when nothing has a thread yet. */
export function projectDetail(p: BoardProject): string {
  const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`
  const open = p.decisions > 0 ? ` · ${plural(p.decisions, 'decision', 'decisions')} open` : ''
  return `${p.threads > 0 ? plural(p.threads, 'thread', 'threads') : plural(p.agents, 'agent', 'agents')}${open}`
}
