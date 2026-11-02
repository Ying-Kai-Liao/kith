import type { Bot } from './models.ts'

/**
 * Quick chat: a new thread with Kith's built-in `scratch` agent. The engine and model are picked per chat and kept
 * by the host for that thread alone.
 */

export interface QuickEngineOption {
  id: string
  label: string
}

/** What the picker shows when `kith engines --json` can't be read: scratch's own, Claude and Codex. */
export const QUICK_ENGINES: QuickEngineOption[] = [
  { id: '', label: 'Default' },
  { id: 'claude', label: 'Claude' },
  { id: 'codex', label: 'Codex' },
]

/**
 * The picker's engines from `kith engines --json`: the ones installed on the computer that have an ACP mode, in the
 * CLI's order, after scratch's own. Throws when the answer isn't that list (an older kith prints only id and installed).
 */
export function parseQuickEngines(raw: unknown): QuickEngineOption[] {
  const rows = (raw as { engines?: unknown } | null)?.engines
  if (!Array.isArray(rows)) throw new Error('`kith engines --json` did not list engines')
  const known = rows.filter((r): r is Record<string, unknown> => typeof r === 'object' && r !== null)
  if (!known.some((r) => Array.isArray(r.acp))) throw new Error('this kith does not say which engines have an ACP mode; update kith')
  const out = known
    .filter((r) => typeof r.id === 'string' && r.id !== '' && r.installed === true && Array.isArray(r.acp) && r.acp.length > 0)
    .map((r) => ({ id: r.id as string, label: typeof r.name === 'string' && r.name ? r.name : (r.id as string) }))
  return [QUICK_ENGINES[0]!, ...out]
}

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
