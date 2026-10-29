import type { CSSProperties } from 'react'
import { parseColor } from '../../../shared/living-bots.ts'

/** An agent's two-colour pair (the `kith:` palette): a thread's line, the "1 new" pill, a decision's buttons. */
export interface Pair {
  a: string
  b: string
}

/** For a bot with no palette pick (empty or an old bare id): a calm sky blue, never violet. */
export const FALLBACK_PAIR: Pair = { a: '#8fd0f7', b: '#5aa7e8' }

/** Not an agent's: a bot from outside has no colour of its own, so its card tints with the neutral muted and line (iOS AgentTint.neutral). */
export const NEUTRAL_PAIR: Pair = { a: 'var(--kith-muted)', b: 'var(--kith-line)' }

export function pairOf(avatarColor: string | undefined | null): Pair {
  const hit = parseColor(avatarColor)
  if (!hit) return FALLBACK_PAIR
  // Plain is drawn flat: the pair's `a` on both stops.
  return hit.plain ? { a: hit.pair.a, b: hit.pair.a } : { a: hit.pair.a, b: hit.pair.b }
}

/** Sets `--pair-a` / `--pair-b` for kith.css to tint from. */
export const pairStyle = (pair: Pair): CSSProperties => ({ '--pair-a': pair.a, '--pair-b': pair.b }) as CSSProperties
