// Where the chat's one box sends (client side): a task's Reply points it at that task (its root as the thread), "New
// task" at a new one. Unset, the box goes on with the newest task as the host would (`continuingTask`).

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

const targets = new WeakMap<object, TaskTargets>()
/** The task targets of a store (one per computer). */
export function targetsOf(store: object): TaskTargets {
  let t = targets.get(store)
  if (!t) targets.set(store, (t = new TaskTargets()))
  return t
}
