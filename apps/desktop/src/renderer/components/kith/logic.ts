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

/** A Kith default profile picture: two palette colours (top to bottom) behind a zoomed crop of the sky art's clouds. */
export interface SkyLook {
  name: string
  top: string
  bottom: string
  /** How many picture-widths of sky art span the tile; higher is a tighter crop. */
  zoom: number
  /** Which part of the sky art the crop shows, in percent of the movable range (CSS background-position). */
  x: number
  y: number
  /** How strongly the clouds show over the palette, in percent. */
  cloud: number
}

/** The eight Kith default profile pictures, the same on iOS (ProfileImageTests pins both). Keep these exact. */
export const SKY_PRESETS: readonly SkyLook[] = [
  { name: 'night', top: '#050A24', bottom: '#1B2F7A', zoom: 3, x: 0, y: 0, cloud: 30 },
  { name: 'golden', top: '#E8801A', bottom: '#FFD27A', zoom: 3, x: 100, y: 0, cloud: 55 },
  { name: 'dusk', top: '#4A2F98', bottom: '#F07A5A', zoom: 4, x: 90, y: 10, cloud: 50 },
  { name: 'dawn', top: '#6FA0E8', bottom: '#F8B890', zoom: 4, x: 5, y: 15, cloud: 55 },
  { name: 'noon', top: '#0F4FC8', bottom: '#4FA8F2', zoom: 3, x: 80, y: 40, cloud: 60 },
  { name: 'overcast', top: '#58626E', bottom: '#AEB6BE', zoom: 2, x: 40, y: 0, cloud: 75 },
  { name: 'twilight', top: '#141C5E', bottom: '#C8409A', zoom: 4, x: 10, y: 30, cloud: 45 },
  { name: 'mint', top: '#1E8A78', bottom: '#BDEBD0', zoom: 3, x: 85, y: 70, cloud: 60 },
]

/** Where the sky art sits on an `out`-pixel square, by the CSS background-size / -position rules. */
export function skyCropRect(imgW: number, imgH: number, pick: { x: number; y: number }, out: number, zoom = 4.2) {
  const w = out * zoom
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
