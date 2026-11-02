import type { Bot } from './models.ts'

/**
 * Quick chat: a new thread with Kith's built-in `scratch` agent. The engine and model are picked per chat and kept
 * by the host for that thread alone.
 */

/** The engines the host starts for a chat (the host refuses others). `''` is scratch's own. */
export const QUICK_ENGINES: { id: '' | 'claude' | 'codex'; label: string }[] = [
  { id: '', label: 'Default' },
  { id: 'claude', label: 'Claude' },
  { id: 'codex', label: 'Codex' },
]

export interface QuickEngine {
  engine?: string
  engineModel?: string
}

/** What a chat's engine picks send: nothing when both are left at scratch's own. */
export function quickEngine(engine: string, model: string): QuickEngine | null {
  const e = engine.trim()
  const m = model.trim()
  if (!e && !m) return null
  return { engine: e || 'claude', ...(m ? { engineModel: m } : {}) }
}

/** The bot Quick chat talks to: the built-in scratch agent's. A user's own agent called scratch is an ordinary bot. */
export function quickChatBot(bots: readonly Bot[]): Bot | undefined {
  return bots.find((b) => b.builtin === true && b.hqAgent === 'scratch' && !b.hidden && b.kind !== 'group' && !b.botGuest)
}

/** Why a name can't be an agent's, before the host is asked; null when it can. The host and kith have the last word. */
export function keepNameError(name: string): string | null {
  const n = name.trim()
  if (!n) return 'Name the agent.'
  if (n.length > 64) return 'Use at most 64 characters.'
  if (/\s/.test(n) || n.startsWith('-')) return 'Use letters, numbers and dashes, with no spaces.'
  return null
}
