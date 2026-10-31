import { useEffect, useState } from 'react'
import { fetchTargets, type LinkPreview } from '@shared/link-preview'
import { prefs, usePref } from './prefs'

// Pictures for News rows. The main process fetches and caches them; this only asks, once per URL per window.
const settled = new Map<string, LinkPreview>()
const asked = new Map<string, Promise<LinkPreview>>()

/** Asks for `url`'s preview (a no-op when it was already asked); resolves with nothing if the bridge lacks the call. */
export function loadPreview(url: string): Promise<LinkPreview> {
  let p = asked.get(url)
  if (!p) {
    const call = window.kith.app.linkPreview
    p = (typeof call === 'function' ? call(url) : Promise.resolve({ image: null, icon: null })).catch(() => ({ image: null, icon: null }))
    void p.then((v) => settled.set(url, v))
    asked.set(url, p)
  }
  return p
}

/** Whether News pictures are on for this card: the Settings switch, and only in the owner's own chats. */
export function useLinkPictures(isOwner: boolean): boolean {
  const [on] = usePref(prefs.linkPictures)
  return on && isOwner
}

/** Starts every row's fetch when the card mounts, so the rows behind "Show N more" are ready when opened. */
export function usePrefetch(urls: string[], enabled: boolean) {
  const key = urls.join('\n')
  useEffect(() => {
    for (const u of fetchTargets(key.split('\n'), enabled)) void loadPreview(u)
  }, [key, enabled])
}

/** The preview for `url`, or null while loading, off or empty. `late` is true when it arrived after first paint. */
export function useLinkPreview(url: string, enabled: boolean): { preview: LinkPreview | null; late: boolean } {
  const [initial] = useState(() => settled.get(url) ?? null)
  const [got, setGot] = useState<LinkPreview | null>(initial)
  useEffect(() => {
    if (!enabled) return
    let live = true
    void loadPreview(url).then((v) => live && setGot(v))
    return () => void (live = false)
  }, [url, enabled])
  if (!enabled) return { preview: null, late: false }
  return { preview: got, late: got !== null && initial === null }
}
