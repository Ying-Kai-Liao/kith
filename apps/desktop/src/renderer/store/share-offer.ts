import type { SharedItem } from '@shared/share-link'
import { Observable } from '../lib/observable'

/** What was shared to Kith from another app, offered to a chat until it is sent or closed. */
export class ShareOffer extends Observable {
  item: SharedItem | null = null

  /** Something arrived (or was waiting at launch): offers it. */
  async start() {
    const item = await window.kith.shares.takePending()
    if (!item) return
    this.item = item
    this.changed()
  }

  dismiss() {
    this.item = null
    this.changed()
  }
}

export const shareOffer = new ShareOffer()
