// Link pictures for the News card (DESIGN.md "News"): what a page's <head> says about its preview picture and icon,
// which hosts the app refuses to visit, and the on-device cache's rules. Pure; the main process does the fetching.

export interface LinkPreview {
  /** A data: URL of the page's preview picture, or null. */
  image: string | null
  /** A data: URL of the page's icon, or null. */
  icon: string | null
}

export const NO_PREVIEW: LinkPreview = { image: null, icon: null }

/** A picture whose short side is under this is treated as no picture (DESIGN.md). */
export const MIN_SIDE = 88

export interface HeadLinks {
  /** In order of preference: og:image, then twitter:image. The apple-touch-icon is the last resort. */
  images: string[]
  touchIcons: string[]
  icons: string[]
}

const ENTITIES: Record<string, string> = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ' }
export function decodeEntities(s: string): string {
  return s.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (m, e: string) => {
    if (e[0] === '#') {
      const code = e[1] === 'x' || e[1] === 'X' ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10)
      return Number.isFinite(code) && code > 0 && code <= 0x10ffff ? String.fromCodePoint(code) : m
    }
    return ENTITIES[e.toLowerCase()] ?? m
  })
}

function attrsOf(tag: string): Record<string, string> {
  const out: Record<string, string> = {}
  for (const m of tag.matchAll(/([^\s"'<>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+)))?/g)) {
    const name = m[1]!.toLowerCase()
    if (!(name in out)) out[name] = decodeEntities(m[2] ?? m[3] ?? m[4] ?? '')
  }
  return out
}

function resolve(href: string, base: string): string | null {
  try {
    const u = new URL(href.trim(), base)
    return u.protocol === 'http:' || u.protocol === 'https:' ? u.href : null
  } catch {
    return null
  }
}

/** The picture and icon candidates in a page's <head>; relative URLs resolve against `base`, the final page URL. */
export function parseHead(html: string, base: string): HeadLinks {
  const end = html.search(/<\/head\s*>|<body[\s>]/i)
  const head = end >= 0 ? html.slice(0, end) : html
  const og: string[] = []
  const tw: string[] = []
  const touchIcons: string[] = []
  const icons: string[] = []
  for (const m of head.matchAll(/<(meta|link)\b([^>]*)>/gi)) {
    const a = attrsOf(m[2]!)
    if (m[1]!.toLowerCase() === 'meta') {
      const key = (a.property ?? a.name ?? '').toLowerCase()
      const url = a.content ? resolve(a.content, base) : null
      if (!url) continue
      if (key === 'og:image' || key === 'og:image:url' || key === 'og:image:secure_url') og.push(url)
      else if (key === 'twitter:image' || key === 'twitter:image:src') tw.push(url)
    } else {
      const rels = (a.rel ?? '').toLowerCase().split(/\s+/)
      const url = a.href ? resolve(a.href, base) : null
      if (!url) continue
      if (rels.includes('apple-touch-icon') || rels.includes('apple-touch-icon-precomposed')) touchIcons.push(url)
      else if (rels.includes('icon')) icons.push(url)
    }
  }
  return { images: [...new Set([...og, ...tw])], touchIcons, icons }
}

/** Hosts on the user's own machine or network; a preview must never reach into them. */
export function isPrivateHost(hostname: string): boolean {
  const h = hostname.toLowerCase().replace(/^\[|\]$/g, '').replace(/\.$/, '')
  if (!h || h === 'localhost' || h.endsWith('.localhost') || h.endsWith('.local') || h.endsWith('.internal')) return true
  const v4 = h.match(/^(?:::ffff:)?(\d+)\.(\d+)\.(\d+)\.(\d+)$/)
  if (v4) {
    const [a, b] = [Number(v4[1]), Number(v4[2])]
    return a === 0 || a === 10 || a === 127 || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 100 && b >= 64 && b <= 127) || a >= 224
  }
  if (h.includes(':')) {
    // IPv6: loopback, unspecified, unique-local (fc00::/7), link-local (fe80::/10), v4-mapped.
    return h === '::1' || h === '::' || /^f[cd]/.test(h) || /^fe[89ab]/.test(h) || h.startsWith('::ffff:')
  }
  return false
}

/** Only plain http(s) pages with no embedded credentials and a public host. */
export function isFetchableUrl(url: string): boolean {
  try {
    const u = new URL(url)
    return (u.protocol === 'http:' || u.protocol === 'https:') && !u.username && !u.password && !isPrivateHost(u.hostname)
  } catch {
    return false
  }
}

/** How long a result, a negative one included, is trusted. */
export const CACHE_TTL_MS = 7 * 24 * 3600 * 1000
export const CACHE_MAX_ENTRIES = 400

export interface CacheRecord { at: number; value: LinkPreview }

/** A cache keyed by page URL: hits until they expire, the oldest dropped past the cap. */
export class PreviewCache {
  private map = new Map<string, CacheRecord>()
  constructor(private now: () => number = Date.now, private max = CACHE_MAX_ENTRIES, private ttl = CACHE_TTL_MS) {}
  get(url: string): LinkPreview | undefined {
    const r = this.map.get(url)
    if (!r) return undefined
    if (this.now() - r.at > this.ttl) {
      this.map.delete(url)
      return undefined
    }
    return r.value
  }
  set(url: string, value: LinkPreview, at = this.now()) {
    this.map.delete(url)
    this.map.set(url, { at, value })
    while (this.map.size > this.max) this.map.delete(this.map.keys().next().value as string)
  }
  entries(): [string, CacheRecord][] {
    return [...this.map]
  }
  load(entries: [string, CacheRecord][]) {
    for (const [k, r] of [...entries].sort((a, b) => a[1].at - b[1].at)) this.set(k, r.value, r.at)
  }
}
