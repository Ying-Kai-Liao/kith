// What crosses the bridge for reaching a Mac over the Kith cloud relay (main/relay/): the end-to-end encrypted
// channel itself stays in the main process, the window only sees calls, streams, a state and these shapes.

export interface CloudFailure {
  status: number
  code: string
  message: string | null
}

export type CloudResult<T> = { ok: true; value: T } | { ok: false; error: CloudFailure }

/** Where a relay channel to one Mac stands. */
export type RelayState =
  | { kind: 'connecting' }
  | { kind: 'ready' }
  /** The cloud says the Mac is off; the channel comes back by itself when it is on. */
  | { kind: 'hostOffline'; lastSeenAt: number | null }
  /** Access was removed (or, with `retrying`, the Mac couldn't confirm it just now). Stays until `restart`. */
  | { kind: 'unauthorized'; message: string; retrying?: boolean }
  /** `final`: stays until `restart` (the key doesn't match, the Mac needs an update); otherwise it is retrying. */
  | { kind: 'failed'; message: string; code: string; final: boolean }

/** The error a relay call or stream ends with. `status` 0 is the link, not the request: retrying can work. */
export interface RelayCallError {
  status: number
  message: string
  code: string
}

/** A Mac the account owns or is a member of (`GET /v1/computers`). */
export interface RelayComputer {
  computerId: string
  name: string
  role: 'owner' | 'member'
  platform: string
  signKey: string
  online: boolean
  /** The cloud's word: `granted` once this device may connect, `pending` while it asks, `none` before; null without a device. */
  access: 'granted' | 'pending' | 'none' | null
}

/** A pending request for this device to join a Mac; `code` is the six digits the owner checks on the Mac. */
export interface AccessTicket {
  requestId: string
  expiresAt: number
  code: string
  computerId: string
}

export type AccessProgress = 'pending' | 'approved' | 'denied' | 'expired' | 'cancelled'
