// Living bots: the pure rules behind the animated crew avatars. Art and rig data come from the
// branding agent (living-bots/eyes.json), the timing from the designer (living-bots/tracks.json,
// MOTION.md); the iOS app reads the same files, so a bot id picks the same loops on both.

import eyesJson from './living-bots/eyes.json' with { type: 'json' }
import tracksJson from './living-bots/tracks.json' with { type: 'json' }

export interface Eye {
  x: number
  y: number
  w: number
  h: number
  /** Degrees clockwise from vertical. */
  tilt: number
}

export interface Rig {
  eyes: [Eye, Eye]
  /** How far both eyes may move together, per direction, as a fraction of the box. */
  look: { left: number; right: number; up: number; down: number }
  /** The bottom centre of the visible body: where squash and stretch pivot. */
  anchor: { x: number; y: number }
  /** Whether tint.png can recolour the body (false: the kith cube). */
  tint: boolean
  /** Whether detail.png exists (coder's glyph). */
  detail: boolean
}

export interface PaletteColor {
  id: string
  a: string
  b: string
}

const eyes = eyesJson as unknown as { palette: PaletteColor[]; bots: Record<string, Rig> }

/** The 15 named colour pairs a bot can wear (the default is its untouched body). */
export const PALETTE: readonly PaletteColor[] = eyes.palette

/** What avatarColor holds for a palette pick. Bare ids (`blue`, `green`...) are the old colours, not picks. */
export const PALETTE_PREFIX = 'kith:'
/** Plain (a flat fill of the pair's first colour) is `kith:<id>:plain`; `kith:<id>` stays the gradient. iOS writes the same. */
export const PLAIN_SUFFIX = ':plain'
export const paletteValue = (id: string, plain = false) => `${PALETTE_PREFIX}${id}${plain ? PLAIN_SUFFIX : ''}`

export interface ColorPick {
  pair: PaletteColor
  plain: boolean
}

/** The one parse of an avatarColor: the pair and gradient or plain, or null for the original body (empty, an old bare id, or an id this app doesn't know). */
export function parseColor(value: string | undefined | null): ColorPick | null {
  if (!value?.startsWith(PALETTE_PREFIX)) return null
  let id = value.slice(PALETTE_PREFIX.length)
  const plain = id.endsWith(PLAIN_SUFFIX)
  if (plain) id = id.slice(0, -PLAIN_SUFFIX.length)
  const pair = PALETTE.find((c) => c.id === id)
  return pair ? { pair, plain } : null
}

/** The pair for an avatarColor, or null for the original body. */
export const paletteColor = (value: string | undefined | null): PaletteColor | null => parseColor(value)?.pair ?? null

/** The rig of a crew member, a spare, or `kith` (the cube). */
export function rigOf(name: string): Rig | null {
  return eyes.bots[name] ?? null
}

/** The kith cube on the group tile: visible width 70% of the tile, centred on its bounding box (LAYERS.md section 5). */
export const GROUP_TRANSFORM = { scale: 1.134, dx: 0, dy: 0.0044 }

/** The tile's gradient (logo colours) is laid over #F6F6FA at this opacity. */
export const GROUP_TILE = { base: '#F6F6FA', gradientOpacity: 0.45 }

// ---- clock, seed and desync (MOTION.md section 3) ----

/** FNV-1a, 32 bit, over the UTF-8 bytes of the bot id. */
export function seedOf(id: string): number {
  let h = 0x811c9dc5
  for (const byte of new TextEncoder().encode(id)) {
    h ^= byte
    h = Math.imul(h, 0x01000193) >>> 0
  }
  return h >>> 0
}

const track = (name: string) => (tracksJson.tracks as { name: string; duration: number }[]).find((t) => t.name === name)!.duration

export const LOOK_IDLE_MS = [0, 1, 2, 3].map((i) => track(`look-idle-${i}`))
export const BLINK_MS = [0, 1, 2].map((i) => track(`blink-${i}`))
export const LOOK_WORKING_MS = track('look-working')
export const LOOK_ERROR_MS = track('look-error')
export const BODY_IDLE_MS = track('body-idle')
export const BODY_WORKING_MS = track('body-working')
export const BODY_NEEDS_LOOP_MS = track('body-needs-loop')

export interface Loops {
  lookTrack: number
  blinkTrack: number
  fLook: number
  fBlink: number
  fBody: number
}

/** Which loops a bot plays and where in them it starts, from its id alone. */
export function loopsOf(id: string): Loops {
  const seed = seedOf(id)
  return {
    lookTrack: seed % 4,
    blinkTrack: (seed >>> 2) % 3,
    fLook: ((seed >>> 4) & 0x3ff) / 1024,
    fBlink: ((seed >>> 14) & 0x3ff) / 1024,
    fBody: ((seed >>> 24) & 0xff) / 256,
  }
}

/** The blink tier's idle glance (a list face would only blink otherwise): one look to a side per
 * period, the side picked by the seed's low bit, as on iOS (BotGlance). */
export const GLANCE_MS = 9000
export const glanceSide = (id: string): -1 | 1 => (seedOf(id) & 1 ? 1 : -1)

/** Where in a loop of `length` ms a bot is `now`: the same on every screen showing it. */
export const phaseMs = (length: number, f: number, now: number) => (now + f * length) % length

// ---- size tiers (MOTION.md section 7) ----

export type Tier = 'full' | 'blink' | 'static'

export const FULL_TIER_MIN = 40
export const BLINK_TIER_MIN = 16
/** At most this many avatars run the full tier in one window at once; the rest blink. */
export const MAX_FULL = 6

export function sizeTier(size: number): Tier {
  return size >= FULL_TIER_MIN ? 'full' : size >= BLINK_TIER_MIN ? 'blink' : 'static'
}

/** scaleY of a closed eye: about 0.75 px tall, never below 10%. */
export const closedScale = (eyeH: number, size: number) => Math.max(0.1, 0.75 / (eyeH * size))

// ---- state changes (MOTION.md section 6) ----

/** A bot must have worked this long for the done one-shot to play, and not played it within the gap. */
export const DONE_MIN_WORK_MS = 1000
export const DONE_GAP_MS = 4000
/** A bot that played the needs-you double hop skips it for this long. */
export const NEEDS_ENTER_GAP_MS = 10000

export const doneDue = (workedMs: number, sinceLastDoneMs: number) => workedMs >= DONE_MIN_WORK_MS && sinceLastDoneMs >= DONE_GAP_MS

// ---- colour (LAYERS.md section 4) ----

type Rgb = [number, number, number]

const hexRgb = (hex: string): Rgb => {
  const n = parseInt(hex.slice(1), 16)
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255]
}
const toLinear = (c: number) => {
  const v = c / 255
  return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4
}
const fromLinear = (v: number) => Math.round(255 * Math.max(0, Math.min(1, v <= 0.0031308 ? 12.92 * v : 1.055 * v ** (1 / 2.4) - 0.055)))

export function toOklab([r, g, b]: Rgb): Rgb {
  const [lr, lg, lb] = [toLinear(r), toLinear(g), toLinear(b)]
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb)
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb)
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb)
  return [0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s, 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s, 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s]
}

export function fromOklab([L, a, b]: Rgb): Rgb {
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3
  return [
    fromLinear(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
    fromLinear(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
    fromLinear(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
  ]
}

/** The 256-entry lookup from tint.png's gray level (0 = a, 255 = b) to a colour, blended in OKLab. */
export function gradientTable(a: string, b: string): Rgb[] {
  const from = toOklab(hexRgb(a))
  const to = toOklab(hexRgb(b))
  return Array.from({ length: 256 }, (_, i) => {
    const t = i / 255
    return fromOklab([from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t, from[2] + (to[2] - from[2]) * t])
  })
}

/** Coder's glyph: colour `a` at OKLab lightness x0.55, chroma kept. */
export function glyphColor(a: string): Rgb {
  const [L, A, B] = toOklab(hexRgb(a))
  return fromOklab([L * 0.55, A, B])
}
