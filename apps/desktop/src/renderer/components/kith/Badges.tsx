import { Icon } from '../Icon'
import { countLabel, statusChipOf, type StatusKind } from './logic.ts'
import { pairStyle, type Pair } from './pair.ts'
import './kith.css'

/** Ink pill with the number in white Inter 11 bold, tabular figures. 18 tall (16 on Board). Hidden at 0, `99+` above 99. */
export function CountBadge({ count, size = 18, label, inverted = false }: { count: number; size?: 18 | 16; label?: string; /** White pill with dark digits, for a black ground (the notch island). */ inverted?: boolean }) {
  const text = countLabel(count)
  if (!text) return null
  return (
    <span className={inverted ? 'kc-count inverted' : 'kc-count'} style={{ height: size, minWidth: size }} role="img" aria-label={label ?? `${text} waiting`}>
      {text}
    </span>
  )
}

/** The live dot: the agent's pair, a slow opacity pulse (static under prefers-reduced-motion). */
export function LiveDot({ pair, size = 7 }: { pair: Pair; size?: number }) {
  return <span className="kc-live-dot" style={{ ...pairStyle(pair), width: size, height: size }} aria-hidden />
}

/** A status as a word and a colour, never colour alone. */
export function StatusChip({ kind, detail }: { kind: StatusKind; detail?: string }) {
  const m = statusChipOf(kind, detail)
  return (
    <span className={`kc-chip kc-tone-${m.tone}`}>
      {m.icon ? <Icon name={m.icon} size={10} weight="semibold" /> : <span className="kc-chip-dot" aria-hidden />}
      <span className="kc-chip-word">{m.word}</span>
    </span>
  )
}

/** The writer variant: "In terminal" (muted glyph) or "Working in terminal" (live dot in the agent's pair), with the Mac in a thread header. */
export function WriterChip({ working = false, mac, pair }: { working?: boolean; mac?: string; pair: Pair }) {
  const word = working ? 'Working in terminal' : 'In terminal'
  return (
    <span className={`kc-chip kc-writer ${working ? 'working' : ''}`} title={mac ? `${word} on ${mac}` : word}>
      {working ? <LiveDot pair={pair} /> : <Icon name="terminal" size={10} weight="medium" />}
      <span className="kc-chip-word">{mac ? `${word} · ${mac}` : word}</span>
    </span>
  )
}

export type Origin = 'terminal' | 'discord' | 'telegram' | 'slack' | 'bot'
const ORIGIN_WORD: Record<Origin, string> = { terminal: 'Terminal', discord: 'Discord', telegram: 'Telegram', slack: 'Slack', bot: 'Bot' }

/** Where a thread started: a muted icon and one word. Never coloured, never truncated. Not shown for app threads. */
export function OriginTag({ origin, title, label }: { origin: Origin; title?: string; label?: string }) {
  return (
    <span className="kc-origin" title={title ?? label ?? `Started in ${origin === 'terminal' ? 'a terminal' : ORIGIN_WORD[origin]}`}>
      <Icon name={origin === 'terminal' ? 'terminal' : 'bubble.left.and.bubble.right'} size={10} weight="medium" />
      {label ?? ORIGIN_WORD[origin]}
    </span>
  )
}
