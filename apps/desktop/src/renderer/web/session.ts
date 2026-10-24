import type { Unsupported } from './detect.ts'

/**
 * What the web screens need beyond `window.kith`. bridge.ts provides it when the web entry loads; this module has no
 * side effects, so the screens can sit in the shared renderer bundle without dragging the bridge (or Clerk) into the
 * Electron build or replacing its `window.kith`.
 */
export interface WebSession {
  ready(): Promise<void>
  /** Why this browser can't hold the device key, or null when it can. */
  support(): Promise<Unsupported | null>
  /** The invite waiting to be joined (no token): which computer it is for. Null when none. */
  pendingLink(): Promise<{ computerId: string } | null>
  /** Drops the waiting invite once the join ended or the person closed it. */
  discardPending(): Promise<void>
  /** "Sign in again": ends the Clerk session only. The key and the kept invite stay, so nothing is proven twice. */
  signInAgain(): Promise<void>
}

let current: WebSession | null = null

export const provideWebSession = (session: WebSession) => void (current = session)

const need = () => {
  if (!current) throw new Error('The web bridge is not loaded.')
  return current
}

export const webSession: WebSession = {
  ready: () => need().ready(),
  support: () => need().support(),
  pendingLink: () => need().pendingLink(),
  discardPending: () => need().discardPending(),
  signInAgain: () => need().signInAgain(),
}
