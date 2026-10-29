import { glyphColor, gradientTable, parseColor, rigOf } from '@shared/living-bots'

// The bots' layers (branding living-bots/bots/<name>/), bundled with the app at 256 px.
const layers = import.meta.glob<string>('../assets/bots/*/*.png', { eager: true, query: '?url', import: 'default' })

const layerURL = (name: string, layer: 'body' | 'tint' | 'detail') => layers[`../assets/bots/${name}/${layer}.png`] ?? null

/** The untouched body of a crew member, a spare or `kith`. */
export const bodyURL = (name: string) => layerURL(name, 'body')

const recoloured = new Map<string, string>()
const pending = new Map<string, Promise<string | null>>()

/** The recoloured body if it is already rendered. */
export const cachedRecolour = (name: string, colorId: string) => recoloured.get(`${name}|${colorId}`) ?? null

function load(url: string) {
  return new Promise<HTMLImageElement>((resolve, reject) => {
    const image = new Image()
    image.onload = () => resolve(image)
    image.onerror = reject
    image.src = url
  })
}

function pixels(image: HTMLImageElement) {
  const canvas = document.createElement('canvas')
  canvas.width = image.naturalWidth
  canvas.height = image.naturalHeight
  const ctx = canvas.getContext('2d', { willReadFrequently: true })!
  ctx.drawImage(image, 0, 0)
  return { canvas, ctx, data: ctx.getImageData(0, 0, canvas.width, canvas.height) }
}

/**
 * The body in one of the palette's colour pairs, rendered once per (bot, colour) and kept as a data URL:
 * tint.png's gray level looks up the pair's gradient (OKLab), its alpha stays. Coder's glyph is painted on
 * top in a darker colour. Resolves null for an original body, the kith cube, or when the layers fail to load.
 */
export function recolour(name: string, colorId: string): Promise<string | null> {
  const pick = parseColor(colorId)
  const pair = pick?.pair
  const tint = layerURL(name, 'tint')
  if (!pair || !tint || !rigOf(name)?.tint) return Promise.resolve(null)
  const key = `${name}|${colorId}`
  const hit = recoloured.get(key)
  if (hit) return Promise.resolve(hit)
  let job = pending.get(key)
  if (!job) {
    job = (async () => {
      const [map, glyph] = await Promise.all([load(tint), rigOf(name)!.detail && layerURL(name, 'detail') ? load(layerURL(name, 'detail')!) : null])
      const { canvas, ctx, data } = pixels(map)
      const table = gradientTable(pair.a, pick!.plain ? pair.a : pair.b)
      for (let i = 0; i < data.data.length; i += 4) {
        const [r, g, b] = table[data.data[i]!]!
        data.data[i] = r
        data.data[i + 1] = g
        data.data[i + 2] = b
      }
      ctx.putImageData(data, 0, 0)
      if (glyph) {
        // The glyph's alpha is the mask; its pixels become the darker colour.
        const mask = pixels(glyph)
        const [r, g, b] = glyphColor(pair.a)
        for (let i = 0; i < mask.data.data.length; i += 4) {
          mask.data.data[i] = r
          mask.data.data[i + 1] = g
          mask.data.data[i + 2] = b
        }
        mask.ctx.putImageData(mask.data, 0, 0)
        ctx.drawImage(mask.canvas, 0, 0)
      }
      const url = canvas.toDataURL('image/png')
      recoloured.set(key, url)
      return url
    })().catch(() => null)
    pending.set(key, job)
  }
  return job
}

// ---- menu icons: the body and eyes at rest, drawn on a canvas once per (bot, colour) ----

const iconImages = new Map<string, HTMLImageElement>()
const iconLoading = new Set<string>()
const iconListeners = new Set<() => void>()

/** Calls back when a body used for a menu icon has finished loading, so the icon can be drawn. */
export function onIconsLoaded(callback: () => void) {
  iconListeners.add(callback)
  return () => void iconListeners.delete(callback)
}

/** The body image for a menu icon, or null while it loads (listeners are told when it is ready). */
export function iconBody(name: string, colorId: string): HTMLImageElement | null {
  const tinted = rigOf(name)?.tint && parseColor(colorId) ? colorId : ''
  const key = `${name}|${tinted}`
  const hit = iconImages.get(key)
  if (hit) return hit
  if (!iconLoading.has(key)) {
    iconLoading.add(key)
    void (async () => {
      const url = (tinted ? await recolour(name, tinted) : null) ?? bodyURL(name)
      if (!url) return
      iconImages.set(key, await load(url))
      iconListeners.forEach((callback) => callback())
    })().catch(() => iconLoading.delete(key))
  }
  return null
}

/** The bot at rest in a `size` square: its body, then the two black eyes from the rig. */
export function drawBotIcon(ctx: CanvasRenderingContext2D, name: string, body: HTMLImageElement, size: number) {
  ctx.drawImage(body, 0, 0, size, size)
  ctx.fillStyle = '#000'
  for (const eye of rigOf(name)?.eyes ?? []) {
    ctx.beginPath()
    ctx.ellipse(eye.x * size, eye.y * size, (eye.w * size) / 2, (eye.h * size) / 2, (eye.tilt * Math.PI) / 180, 0, Math.PI * 2)
    ctx.fill()
  }
}
