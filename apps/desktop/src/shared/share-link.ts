/**
 * Something shared to Kith from another app (`kith://share?url=…&title=…&text=…`): the Mac share extension, a Shortcut or
 * `open` builds this link. The iPhone's share sheet makes the same message (KithKit Client/ShareInbox.swift, SharedLink).
 */

export interface SharedItem {
  /** An http(s) address, from `url` or found in `text`. */
  link: string | null
  title: string | null
  text: string | null
}

const MAX = { url: 4096, title: 300, text: 4000, note: 2000 }

const clean = (value: string | null | undefined, max: number): string | null => {
  const trimmed = value?.trim()
  return trimmed ? trimmed.slice(0, max) : null
}

const web = (value: string | null): string | null => {
  if (!value) return null
  try {
    const url = new URL(value)
    return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : null
  } catch {
    return null
  }
}

/** The first web address in shared text ("Look at this https://example.com/a, it's good"). */
export function firstLink(text: string): string | null {
  for (const match of text.matchAll(/https?:\/\/[^\s<>"']+/gi)) {
    const link = web(match[0].replace(/[.,;:!?)\]]+$/, ''))
    if (link) return link
  }
  return null
}

/** What the link carries, or null when `url` isn't a share link or holds nothing to send. */
export function parseShareLink(url: string, schemes: readonly string[]): SharedItem | null {
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return null
  }
  const scheme = parsed.protocol.replace(/:$/, '').toLowerCase()
  if (!schemes.some((s) => s.toLowerCase() === scheme)) return null
  // `kith://share?...` reads as host "share"; some platforms deliver `kith:share?...` or `kith:///share`.
  const where = (parsed.hostname || parsed.pathname.replace(/^\/+/, '')).toLowerCase().replace(/\/+$/, '')
  if (where !== 'share') return null
  const text = clean(parsed.searchParams.get('text'), MAX.text)
  const link = web(clean(parsed.searchParams.get('url'), MAX.url)) ?? (text ? firstLink(text) : null)
  if (!link && !text) return null
  return { link, title: clean(parsed.searchParams.get('title'), MAX.title), text }
}

/** The chat message: the note, then the title or caption when it says something the address doesn't, then the address. */
export function shareMessage(item: SharedItem, note: string): string {
  const [title, text] = [clean(item.title, MAX.title), clean(item.text, MAX.text)]
  const parts = [clean(note, MAX.note)]
  if (item.link) {
    const address = item.link
    parts.push([title, text].find((s) => s && s !== address && !s.includes(address)) ?? null)
    parts.push(address)
  } else {
    parts.push(text)
  }
  return parts.filter((p): p is string => !!p).join('\n\n')
}
