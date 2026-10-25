// The decisions behind the Kith components, as plain functions (no React, no store) so node --test covers them.

/** FNV-1a: a small, stable string hash. Never use the engine's own, which differs between runs. */
export function hashSeed(id: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < id.length; i++) {
    h ^= id.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  return h >>> 0
}

export type SkyTone = 'dawn' | 'clear' | 'dusk'
const TONES: SkyTone[] = ['dawn', 'clear', 'dusk']

export interface SkyPick {
  tone: SkyTone
  /** Which part of the sky image the patch crops, in percent of the image. */
  x: number
  y: number
}

/** The default profile picture for a person: a crop of the sky, seeded by the account id. You are always dawn. */
export function skyPatchOf(id: string, you = false): SkyPick {
  const h = hashSeed(id)
  return {
    tone: you ? 'dawn' : TONES[h % 3]!,
    x: (h >>> 4) % 101,
    y: (h >>> 12) % 101,
  }
}

/** The CountBadge text: nothing for 0 (or less), `99+` above 99. */
export function countLabel(n: number): string | null {
  if (!Number.isFinite(n) || n <= 0) return null
  return n > 99 ? '99+' : String(Math.floor(n))
}

/** The call button sits right of the dock and hides while there is anything to send. */
export function dockCallVisible({ showsCall, hasText, hasAttachments = false }: { showsCall: boolean; hasText: boolean; hasAttachments?: boolean }): boolean {
  return showsCall && !hasText && !hasAttachments
}

/** Mic when the box is empty, send once there is text or an attachment. */
export function dockAction({ hasText, hasAttachments = false }: { hasText: boolean; hasAttachments?: boolean }): 'mic' | 'send' {
  return hasText || hasAttachments ? 'send' : 'mic'
}

export type WriterPhase = 'idle' | 'working' | 'taking' | 'failed' | 'unreachable'

export interface WriterBannerModel {
  icon: 'terminal' | 'dot' | 'spinner' | 'warning'
  title: string
  sentence: string
  /** The one button, if any. */
  button: 'Take over' | 'Try again' | null
}

/** continue-anywhere/spec.md, "Writer banner": what each held-thread state says. Take over is for the terminal's owner only. */
export function writerBannerOf({ phase, mac, mine = true, person }: { phase: WriterPhase; mac: string; mine?: boolean; person?: string }): WriterBannerModel {
  switch (phase) {
    case 'working':
      return { icon: 'dot', title: 'Working in terminal', sentence: `On ${mac}. You can take over when it stops.`, button: null }
    case 'taking':
      return { icon: 'spinner', title: 'Taking over…', sentence: `Closing the terminal session on ${mac}.`, button: null }
    case 'failed':
      return { icon: 'warning', title: "Couldn't take over", sentence: `The terminal on ${mac} still has it.`, button: mine ? 'Try again' : null }
    case 'unreachable':
      return { icon: 'warning', title: `${mac} unreachable`, sentence: "Last seen open in terminal. Reply when it's back.", button: null }
    default:
      return mine
        ? { icon: 'terminal', title: 'Open in terminal', sentence: `On ${mac}. Reply there, or take over.`, button: 'Take over' }
        : { icon: 'terminal', title: person ? `Open in ${person}'s terminal` : 'Open in their terminal', sentence: `On ${mac}.`, button: null }
  }
}

/** The command the take-over dialog shows and Copy puts on the clipboard. */
export const resumeCommand = (agent: string, resumeId: string) => `kith ${agent} --resume ${resumeId}`

export type StatusKind = 'live' | 'catching-up' | 'needs-you' | 'mac-unreachable' | 'urgent' | 'machine'

export interface ChipModel {
  word: string
  /** Which colour the chip's dot and text take; never the only signal, the word is always there. */
  tone: 'positive' | 'muted' | 'ink' | 'negative'
  icon?: string
}

export function statusChipOf(kind: StatusKind, detail?: string): ChipModel {
  switch (kind) {
    case 'live':
      return { word: 'Live', tone: 'positive' }
    case 'catching-up':
      return { word: 'Catching up', tone: 'muted' }
    case 'needs-you':
      return { word: 'Needs you', tone: 'ink' }
    case 'mac-unreachable':
      return { word: detail ? `${detail} unreachable` : 'Mac unreachable', tone: 'negative', icon: 'exclamationmark.triangle' }
    case 'urgent':
      return { word: '/urgent', tone: 'negative' }
    case 'machine':
      return { word: detail ?? 'Mac busy', tone: 'muted' }
  }
}

/** "1 reply" / "N replies". */
export const repliesLabel = (n: number) => `${n} ${n === 1 ? 'reply' : 'replies'}`

/** A bot guest's default picture: Kith's own "G" disc for Grok, the bubble disc for every other or unknown provider. */
export type BotGuestVariant = 'grok' | 'other'
export const botGuestVariant = (provider: string | null | undefined): BotGuestVariant => (provider?.trim().toLowerCase() === 'grok' ? 'grok' : 'other')
