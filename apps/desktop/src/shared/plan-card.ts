// The Plan card (DESIGN.md "Reply cards"): an agent's plan as one report card that changes in place. The host sends
// the steps and what state each is in; the app adds only the count and the bar.

import type { RichCard } from './rich-card.ts'

export type PlanState = 'done' | 'now' | 'next' | 'stopped'
export interface PlanStep { text: string; state: PlanState }

export interface PlanView {
  title: string
  /** "3 of 5", drawn in mono. */
  count: string
  /** 0...1, the bar's fill. */
  fraction: number
  steps: PlanStep[]
  /** The turn is over: nothing spins. */
  ended: boolean
}

const STATES: readonly PlanState[] = ['done', 'now', 'next', 'stopped']
const count = (v: unknown): number => (typeof v === 'number' && Number.isFinite(v) && v > 0 ? Math.floor(v) : 0)

/** A state this app doesn't know reads as the quietest, "next". */
function stepOf(raw: unknown): PlanStep | null {
  if (!raw || typeof raw !== 'object') return null
  const r = raw as Record<string, unknown>
  if (typeof r.text !== 'string' || !r.text) return null
  return { text: r.text, state: STATES.find((s) => s === r.state) ?? 'next' }
}

export function planView(card: RichCard): PlanView {
  const s = card.snapshot ?? {}
  const steps = (Array.isArray(s.steps) ? s.steps : []).map(stepOf).filter((x): x is PlanStep => x !== null)
  const total = Math.max(count(s.total), steps.length)
  const done = Math.min(count(s.done), total)
  return {
    title: typeof s.title === 'string' && s.title ? s.title : 'Plan',
    count: `${done} of ${total}`,
    fraction: total ? done / total : 0,
    steps,
    ended: s.ended === true,
  }
}
