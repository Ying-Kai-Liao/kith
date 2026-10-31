import { createHash } from 'node:crypto'
import { lookup } from 'node:dns/promises'
import { promises as fs } from 'node:fs'
import { join } from 'node:path'
import { app, nativeImage, net } from 'electron'
import { isFetchableUrl, isPrivateHost, MIN_SIDE, NO_PREVIEW, parseHead, PreviewCache, type CacheRecord, type LinkPreview } from '../shared/link-preview'

// The News card's pictures (DESIGN.md "News"). Only the main process fetches: no cookies, short timeouts, small caps,
// no private hosts. The renderer gets data: URLs, so it never makes the request itself.

const PAGE_TIMEOUT_MS = 5000
const IMAGE_TIMEOUT_MS = 5000
const HTML_CAP = 512 * 1024
const IMAGE_CAP = 2 * 1024 * 1024
const MAX_REDIRECTS = 4
const PARALLEL = 4
const DISK_CAP = 50 * 1024 * 1024

const memory = new PreviewCache()
const inflight = new Map<string, Promise<LinkPreview>>()
let running = 0
const waiting: (() => void)[] = []

const dir = () => join(app.getPath('userData'), 'link-previews')
const fileFor = (url: string) => join(dir(), `${createHash('sha1').update(url).digest('hex')}.json`)

/** The host's addresses are public too, so a name that points into the LAN (or rebinds there) is refused. */
async function hostIsPublic(url: URL): Promise<boolean> {
  if (isPrivateHost(url.hostname)) return false
  try {
    const found = await lookup(url.hostname, { all: true })
    return found.length > 0 && found.every((a) => !isPrivateHost(a.address))
  } catch {
    return false
  }
}

/** One GET with no credentials, redirects followed by hand (each hop checked), the body read up to `cap` bytes. */
async function get(start: string, timeoutMs: number, cap: number): Promise<{ url: string; type: string; body: Buffer } | null> {
  const signal = AbortSignal.timeout(timeoutMs)
  let url = start
  for (let hop = 0; hop <= MAX_REDIRECTS; hop++) {
    if (!isFetchableUrl(url) || !(await hostIsPublic(new URL(url)))) return null
    const res = await net.fetch(url, { credentials: 'omit', redirect: 'manual', signal, headers: { accept: 'text/html,image/*;q=0.9,*/*;q=0.5' } })
    if (res.status >= 300 && res.status < 400) {
      const location = res.headers.get('location')
      if (!location) return null
      url = new URL(location, url).href
      continue
    }
    if (!res.ok || !res.body) return null
    const chunks: Buffer[] = []
    let size = 0
    const reader = res.body.getReader()
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      chunks.push(Buffer.from(value))
      size += value.byteLength
      if (size >= cap) {
        void reader.cancel()
        break
      }
    }
    return { url, type: (res.headers.get('content-type') ?? '').toLowerCase(), body: Buffer.concat(chunks).subarray(0, cap) }
  }
  return null
}

/** An image the page names, as a data: URL; `minSide` drops small ones (SVG and other unmeasurable types fail it). */
async function dataUrlOf(url: string, minSide: number): Promise<string | null> {
  try {
    const got = await get(url, IMAGE_TIMEOUT_MS, IMAGE_CAP)
    if (!got || !got.type.startsWith('image/') || got.body.length === 0) return null
    // A body that hit the cap is cut off and would not decode as a whole picture.
    if (got.body.length >= IMAGE_CAP) return null
    if (minSide > 0) {
      const { width, height } = nativeImage.createFromBuffer(got.body).getSize()
      if (Math.min(width, height) < minSide) return null
    }
    return `data:${got.type.split(';')[0]};base64,${got.body.toString('base64')}`
  } catch {
    return null
  }
}

/** null when the page could not be reached (offline, timeout): that is not worth remembering as "no picture". */
async function fetchPreview(pageUrl: string): Promise<LinkPreview | null> {
  try {
    const page = await get(pageUrl, PAGE_TIMEOUT_MS, HTML_CAP)
    if (!page) return null
    if (!page.type.includes('html')) return NO_PREVIEW
    const links = parseHead(page.body.toString('utf8'), page.url)
    let image: string | null = null
    for (const candidate of [...links.images, ...links.touchIcons.slice(0, 1)]) {
      image = await dataUrlOf(candidate, MIN_SIDE)
      if (image) break
    }
    let icon: string | null = null
    for (const candidate of [...links.icons, new URL('/favicon.ico', page.url).href]) {
      icon = await dataUrlOf(candidate, 0)
      if (icon) break
    }
    return { image, icon }
  } catch {
    return null
  }
}

async function readDisk(url: string): Promise<LinkPreview | undefined> {
  try {
    const rec = JSON.parse(await fs.readFile(fileFor(url), 'utf8')) as CacheRecord & { url: string }
    if (rec.url !== url) return undefined
    memory.set(url, rec.value, rec.at)
    return memory.get(url)
  } catch {
    return undefined
  }
}

let pruned = false
/** Once per launch: the oldest files go until the folder is under its cap. */
async function prune() {
  if (pruned) return
  pruned = true
  try {
    const names = await fs.readdir(dir())
    const files = await Promise.all(names.map(async (n) => ({ n, s: await fs.stat(join(dir(), n)) })))
    let total = files.reduce((sum, f) => sum + f.s.size, 0)
    for (const f of files.sort((a, b) => a.s.mtimeMs - b.s.mtimeMs)) {
      if (total <= DISK_CAP) break
      await fs.rm(join(dir(), f.n), { force: true })
      total -= f.s.size
    }
  } catch {}
}

async function writeDisk(url: string, value: LinkPreview) {
  try {
    await fs.mkdir(dir(), { recursive: true })
    await fs.writeFile(fileFor(url), JSON.stringify({ url, at: Date.now(), value }))
    void prune()
  } catch {}
}

async function slot<T>(work: () => Promise<T>): Promise<T> {
  if (running >= PARALLEL) await new Promise<void>((resolve) => waiting.push(resolve))
  running++
  try {
    return await work()
  } finally {
    running--
    waiting.shift()?.()
  }
}

/** The preview for a News row's link: cached on this device, one fetch shared by every caller of the same URL. */
export function linkPreview(url: unknown): Promise<LinkPreview> {
  if (typeof url !== 'string' || url.length > 2048 || !isFetchableUrl(url)) return Promise.resolve(NO_PREVIEW)
  const hit = memory.get(url)
  if (hit) return Promise.resolve(hit)
  const pending = inflight.get(url)
  if (pending) return pending
  const job = (async () => {
    const disk = await readDisk(url)
    if (disk) return disk
    const fresh = await slot(() => fetchPreview(url))
    if (!fresh) return NO_PREVIEW
    memory.set(url, fresh)
    void writeDisk(url, fresh)
    return fresh
  })().finally(() => inflight.delete(url))
  inflight.set(url, job)
  return job
}
