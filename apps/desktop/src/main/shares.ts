import { ipcMain } from 'electron'
import { parseShareLink, type SharedItem } from '../shared/share-link'

/**
 * Something shared to Kith (`kith://share?...`, from the Mac share extension, a Shortcut or `open`). It is held here
 * until the window takes it, the way an invite link is, so one that arrives while the app is starting is not lost.
 */

let pending: SharedItem | null = null
let notify: (() => void) | null = null

/** The window to nudge when something arrives. */
export function onShare(cb: () => void) {
  notify = cb
  if (pending) cb()
}

/** Takes the item if `url` is a share link. */
export function handleShareURL(url: string, schemes: readonly string[]): boolean {
  const item = parseShareLink(url, schemes)
  if (!item) return false
  pending = item
  notify?.()
  return true
}

export function registerShareIPC() {
  ipcMain.handle('share:takePending', () => {
    const item = pending
    pending = null
    return item
  })
}
