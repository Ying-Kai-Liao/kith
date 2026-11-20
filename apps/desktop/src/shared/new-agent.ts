/**
 * "New agent": every bot is a kith agent, made through the host's `newKithAgent` (`kith new --json`). The agent needs a
 * name and a one-line description up front; the host and kith have the last word on both.
 */

import { HARNESSES } from './bot-sections.ts'

/** The name rules the host enforces: 2-31 chars, lowercase letters, digits and dashes, starting with a letter, not "kith". */
export function newAgentNameError(name: string): string | null {
  const n = name.trim()
  if (!n) return 'Name the agent.'
  if (n.length < 2 || n.length > 31) return 'Use 2 to 31 characters.'
  if (!/^[a-z]/.test(n)) return 'Start with a lowercase letter.'
  if (!/^[a-z0-9-]+$/.test(n)) return 'Use lowercase letters, numbers and dashes only.'
  if (n === 'kith') return 'That name is kept for Kith itself.'
  return null
}

/** One line, no ": ", no leading dash. */
export function newAgentDescriptionError(description: string): string | null {
  const d = description.trim()
  if (!d) return 'Say in a line what the agent does.'
  if (/[\r\n]/.test(d)) return 'Keep it to one line.'
  if (d.startsWith('-')) return "Don't start with a dash."
  if (d.includes(': ')) return 'Leave out ": " (a colon and a space).'
  return null
}

export interface NewAgentForm {
  name: string
  description: string
  /** `''` is kith's own default. */
  harness: string
  model: string
  /** `''` or `default` is the agent's own. */
  effort: string
}

/** What "Make a kith agent like this" fills in from a flagged native row. The name is the row's own: the form turns it into a valid one. */
export interface NewAgentPrefill {
  name: string
  description: string
  harness: string
  model: string
}

/** A flagged row's name, description, engine and model. An engine kith can't run (a registry agent, a custom command) is left at kith's default. */
export function prefillOf(bot: { name: string; description: string; backend: string; model?: string | null }): NewAgentPrefill {
  const harness = HARNESSES.some((h) => h.id === bot.backend) ? bot.backend : ''
  // The description is one line in kith: the first line of the row's standing instructions, without ": " (kith's own rule).
  const description = (bot.description.split(/\r?\n/).find((l) => l.trim()) ?? '').trim().replace(/^-+\s*/, '').replace(/: /g, ' - ')
  return { name: bot.name, description, harness, model: harness ? (bot.model ?? '') : '' }
}

/** The `newKithAgent` params: optional picks are left out when untouched. */
export function newAgentParams(f: NewAgentForm): Record<string, string> {
  const params: Record<string, string> = { name: f.name.trim(), description: f.description.trim() }
  if (f.harness) params.harness = f.harness
  if (f.model.trim()) params.model = f.model.trim()
  if (f.effort && f.effort !== 'default') params.effort = f.effort
  return params
}

export interface NewAgentResult<B> {
  agent: string
  botId: string | null
  bot: B | null
}

/** The host's answer; `bot` and `botId` are null while the roster has not picked the agent up or its engine isn't one the host runs. */
export function parseNewAgentResult<B extends { id: string }>(raw: unknown): NewAgentResult<B> {
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>
  const bot = r.bot && typeof r.bot === 'object' ? (r.bot as B) : null
  return {
    agent: typeof r.agent === 'string' ? r.agent : '',
    botId: bot?.id ?? (typeof r.botId === 'string' ? r.botId : null),
    bot,
  }
}

/** `kernelStatus`: whether the kernel can run, and when it can't, what is wrong and what to do. */
export interface KernelStatus {
  ok: boolean
  bin: string | null
  problem: string | null
  hint: string | null
}

export function parseKernelStatus(raw: unknown): KernelStatus {
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>
  const text = (v: unknown) => (typeof v === 'string' && v.trim() ? v : null)
  return { ok: r.ok === true, bin: text(r.bin), problem: text(r.problem), hint: text(r.hint) }
}

export const KERNEL_FALLBACK_PROBLEM = "Kith's kernel can't run, so agents can't start."
export const KERNEL_FALLBACK_HINT = 'Run `kith doctor` in Terminal to see what is wrong.'

/** The host made the agent but a later step failed: its words start with "<agent> was made, but". */
export function madeButFailed(message: string, name: string): boolean {
  return message.startsWith(`${name} was made, but`)
}

export const engineNotRunHere = (name: string) =>
  `${name} was made. Its engine isn't one this app runs yet, so open it from Terminal with \`kith ${name}\`.`
