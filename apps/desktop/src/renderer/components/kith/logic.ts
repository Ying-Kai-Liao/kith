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

/** The eight Kith default profile pictures, the same on iOS: crops of the sky art. Keep these exact. */
export const SKY_PRESETS: readonly SkyPick[] = [
  { tone: 'dawn', x: 20, y: 30 },
  { tone: 'dawn', x: 70, y: 60 },
  { tone: 'clear', x: 15, y: 20 },
  { tone: 'clear', x: 60, y: 40 },
  { tone: 'clear', x: 85, y: 75 },
  { tone: 'dusk', x: 25, y: 50 },
  { tone: 'dusk', x: 75, y: 30 },
  { tone: 'dusk', x: 50, y: 85 },
]

/** The tone washes of `.kc-sky-*` in kith.css as canvas stops (top to bottom), so a rendered preset matches the CSS patch. */
export const SKY_TONE_STOPS: Record<SkyTone, readonly (readonly [number, string])[]> = {
  dawn: [[0, 'rgba(255,190,150,0)'], [0.7, 'rgba(255,170,130,0.55)'], [1, 'rgba(240,130,110,0.7)']],
  clear: [[0, 'rgba(120,190,255,0.1)'], [1, 'rgba(255,255,255,0.12)']],
  dusk: [[0, 'rgba(90,100,190,0.5)'], [0.65, 'rgba(200,120,160,0.45)'], [1, 'rgba(255,170,110,0.5)']],
}

/** Where `.kc-sky` puts the image on an `out`-pixel square: background-size 420% auto, positioned by x/y percent. */
export function skyCropRect(imgW: number, imgH: number, pick: SkyPick, out: number) {
  const w = out * 4.2
  const h = (w * imgH) / imgW
  return { x: (out - w) * (pick.x / 100), y: (out - h) * (pick.y / 100), w, h }
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
