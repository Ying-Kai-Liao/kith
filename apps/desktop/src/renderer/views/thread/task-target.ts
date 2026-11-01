// Where the chat's one box sends (client side): a task's Reply points it at that task (its root as the thread), "New
// task" at a new one. Unset, the box goes on with the newest task as the host would (`continuingTask`).

import { useSyncExternalStore } from 'react'
import type { Entry } from '@shared/models'
import { continuingTask, isTask } from '@shared/tasks'
import type { BotStore } from '../../store/bot-store'

export type TaskTarget = string | 'new'

/** Each chat's chosen task, outside the views so a card or a message row can set it. */
export class TaskTargets {
  private readonly targets = new Map<string, TaskTarget>()
  private readonly listeners = new Set<() => void>()
  private version = 0

  subscribe = (fn: () => void) => {
    this.listeners.add(fn)
    return () => void this.listeners.delete(fn)
  }
  getVersion = () => this.version
  get(botId: string): TaskTarget | null {
    return this.targets.get(botId) ?? null
  }
  set(botId: string, target: TaskTarget | null) {
    if (target === this.get(botId)) return
    if (target) this.targets.set(botId, target)
    else this.targets.delete(botId)
    this.version++
    for (const fn of this.listeners) fn()
  }
}

/**
 * Where the box sends now: the task a Reply picked, else the one it goes on with (`continuingTask`); null starts a new
 * task. `picked` says the user chose it, which is what lets a send take it over from a terminal.
 */
export function useDestination(store: BotStore, botId: string): { going: Entry | null; picked: boolean; newTask: boolean; target: TaskTarget | null } {
  const t = targetsOf(store)
  useSyncExternalStore(t.subscribe, t.getVersion)
  const bot = store.bots.get(botId)
  if (!bot?.tasks) return { going: null, picked: false, newTask: false, target: null }
  const target = t.get(botId)
  const chat = store.chat(botId)
  const chosen = target && target !== 'new' ? chat.find((e) => e.id === target && isTask(e)) ?? null : null
  const going = chosen ?? continuingTask(bot, chat, Date.now())
  return { going, picked: !!chosen, newTask: target === 'new', target }
}

const targets = new WeakMap<object, TaskTargets>()
/** The task targets of a store (one per computer). */
export function targetsOf(store: object): TaskTargets {
  let t = targets.get(store)
  if (!t) targets.set(store, (t = new TaskTargets()))
  return t
}
