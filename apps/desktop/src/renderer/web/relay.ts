import type { KithBridge } from '../../shared/ipc.ts'
import type { AccessProgress, AccessTicket, CloudResult, RelayCallError, RelayComputer, RelayState } from '../../shared/relay.ts'
import { requestAccess } from '../../shared/relay-core/access.ts'
import type { Signer } from '../../shared/relay-core/crypto.ts'
import { browserDial } from '../../shared/relay-core/dial.ts'
import { RelayError, RelayLink } from '../../shared/relay-core/link.ts'
import type { WebCloud } from './cloud.ts'

/**
 * Reaching Macs over the Kith cloud relay from the page: the desktop's main/relay/manager.ts over the shared relay core
 * with the browser dialer (the signature and the session JWT ride in `Sec-WebSocket-Protocol`, design §4.6). The page
 * is the one process here, so the links live in it; the device key signs through WebCrypto and never leaves it.
 */

const failure = (status: number, code: string, message: string): CloudResult<never> => ({ ok: false, error: { status, code, message } })

const asError = (e: unknown): RelayCallError =>
  e instanceof RelayError ? { status: e.status, message: e.message, code: e.code } : { status: 0, message: "Can't reach your computer.", code: 'unreachable' }

export const DEVICE_RETIRED = "This browser was signed out of Kith. Reload this page to set it up again."

export interface WebRelayDeps {
  cloud: WebCloud
  cloudURL: string
  /** Who is signed in and with which Clerk session; the pair is what a registration binds to (null: signed out). */
  session(): { userId: string; sid: string } | null
  /** A fresh session JWT for a socket upgrade; null when there is definitely no session, a throw for a network blip. */
  jwt(): Promise<string | null>
  /** The signed-in user's device key, made when `create`. */
  signer(create: boolean): Promise<Signer | null>
  /** The cloud refused the key (retired or revoked): drop it so the next load makes a new one. */
  keyRefused(userId: string): Promise<void>
  deviceName: string
  WebSocket?: Parameters<typeof browserDial>[0]['WebSocket']
}

export class WebRelay {
  private owner: string | null = null
  /** Bumped whenever the links are dropped (sign-out, another session, a refused key): a `linkFor` that began earlier must not create one. */
  private generation = 0
  private readonly links = new Map<string, RelayLink>()
  private readonly registered = new Map<string, Promise<CloudResult<unknown>>>()
  private readonly asking = new Map<string, { requestId: string | null; cancelled: boolean }>()
  private readonly listeners = new Set<(computerId: string, state: RelayState) => void>()

  private readonly d: WebRelayDeps

  constructor(deps: WebRelayDeps) {
    this.d = deps
  }

  /** Closes every socket (sign-out, §4.2 step 1) and forgets what was registered. */
  closeAll() {
    this.generation++
    for (const l of this.links.values()) l.close()
    this.links.clear()
    this.registered.clear()
    for (const a of this.asking.values()) a.cancelled = true
    this.asking.clear()
    this.owner = null
  }

  /**
   * The device key is registered once per (user, Clerk session) and per tab: a second caller joins the first one's
   * request, so two parts of the page never race two registrations. A new sign-in has a new sid, so it registers again
   * and the row re-binds to it (§4.5), without any new proof.
   */
  ensureDevice(): Promise<CloudResult<{ userId: string }>> {
    const session = this.d.session()
    if (!session) return Promise.resolve(failure(0, 'signedOut', 'Sign in again to reach your computers.'))
    return this.register(session)
  }

  private async register(session: { userId: string; sid: string }): Promise<CloudResult<{ userId: string }>> {
    const key = `${session.userId}:${session.sid}`
    if (this.owner !== key) {
      if (this.owner !== null) this.generation++
      for (const l of this.links.values()) l.close()
      this.links.clear()
      this.registered.clear()
      this.owner = key
    }
    let done = this.registered.get(key)
    if (!done) {
      done = this.d.cloud.request('POST', 'v1/devices', { name: this.d.deviceName, platform: 'web' }, true)
      this.registered.set(key, done)
    }
    const res = await done
    if (res.ok) return { ok: true, value: { userId: session.userId } }
    this.registered.delete(key)
    // 403: this key was retired or revoked. It can never be used again, so it goes, and the next load is a new device.
    if (res.error.status === 403) {
      this.generation++
      for (const l of this.links.values()) l.close()
      this.links.clear()
      await this.d.keyRefused(session.userId)
      return failure(403, 'deviceRetired', DEVICE_RETIRED)
    }
    return res
  }

  private async linkFor(computerId: string, signKey: string): Promise<RelayLink | RelayCallError> {
    const generation = this.generation
    const gone = { status: 0, message: 'Sign in again to reach your computers.', code: 'signedOut' }
    const ensured = await this.ensureDevice()
    if (!ensured.ok) return { status: ensured.error.status, message: ensured.error.message ?? 'Sign in again.', code: ensured.error.code }
    if (generation !== this.generation) return gone
    const signer = await this.d.signer(false)
    if (!signer || generation !== this.generation) return gone
    const existing = this.links.get(computerId)
    if (existing) return existing
    const link = new RelayLink({
      cloudURL: this.d.cloudURL,
      computerId,
      hostSignKey: signKey,
      signer,
      dial: browserDial({ jwt: this.d.jwt, WebSocket: this.d.WebSocket }),
    })
    link.onState((state) => {
      for (const cb of this.listeners) cb(computerId, state)
    })
    this.links.set(computerId, link)
    return link
  }

  readonly bridge: KithBridge['relay'] = {
    computers: async (): Promise<CloudResult<RelayComputer[]>> => {
      const ensured = await this.ensureDevice()
      if (!ensured.ok) return ensured
      const res = await this.d.cloud.request('GET', 'v1/computers', undefined, true)
      if (!res.ok) return res
      const rows = ((res.value as { computers?: Record<string, unknown>[] }).computers ?? []).filter((c) => typeof c.computerId === 'string')
      return {
        ok: true,
        value: rows.map((c) => ({
          computerId: c.computerId as string,
          name: typeof c.name === 'string' ? c.name : 'Mac',
          role: c.role === 'member' ? 'member' : 'owner',
          platform: typeof c.platform === 'string' ? c.platform : '',
          signKey: typeof c.signKey === 'string' ? c.signKey : '',
          online: c.online === true,
          access: c.access === 'granted' || c.access === 'pending' || c.access === 'none' ? c.access : null,
        })),
      }
    },

    call: async (computerId, signKey, method, body, timeoutMs) => {
      const link = await this.linkFor(computerId, signKey)
      if (!(link instanceof RelayLink)) throw link
      try {
        return await link.call(method, body, timeoutMs)
      } catch (e) {
        throw asError(e)
      }
    },

    stream: (computerId, signKey, sub, body, onData, onEnd) => {
      let cancel: (() => void) | null = null
      let closed = false
      void this.linkFor(computerId, signKey).then((link) => {
        if (closed) return
        if (!(link instanceof RelayLink)) return onEnd(link)
        cancel = link.subscribe(sub, body, onData, (error) => onEnd(error ? asError(error) : null))
      })
      return () => {
        closed = true
        cancel?.()
      }
    },

    state: async (computerId, signKey): Promise<RelayState> => {
      const link = await this.linkFor(computerId, signKey)
      if (!(link instanceof RelayLink)) return { kind: 'failed', message: link.message, code: link.code, final: false }
      link.start()
      return link.state
    },

    onState: (cb) => {
      this.listeners.add(cb)
      return () => void this.listeners.delete(cb)
    },

    restart: async (computerId) => {
      this.links.get(computerId)?.restart()
    },

    forget: async (computerId) => {
      this.links.get(computerId)?.close()
      this.links.delete(computerId)
    },

    requestAccess: async (computerId, signKey): Promise<CloudResult<AccessTicket>> => {
      const ensured = await this.ensureDevice()
      if (!ensured.ok) return ensured
      const signer = await this.d.signer(false)
      if (!signer) return failure(0, 'signedOut', 'Sign in again to reach your computers.')
      const earlier = this.asking.get(computerId)
      if (earlier) earlier.cancelled = true
      const mine = { requestId: null as string | null, cancelled: false }
      this.asking.set(computerId, mine)
      try {
        return await requestAccess({
          request: (m, p, b, s) => this.d.cloud.request(m, p, b, s),
          signer,
          computerId,
          signKey,
          cancelled: () => mine.cancelled,
          created: (id) => (mine.requestId = id),
        })
      } finally {
        if (this.asking.get(computerId) === mine) this.asking.delete(computerId)
      }
    },

    accessStatus: async (requestId): Promise<CloudResult<AccessProgress>> => {
      if (!/^[A-Za-z0-9_-]{1,128}$/.test(requestId)) return failure(400, 'badRequest', 'Bad request id.')
      const res = await this.d.cloud.request('GET', `v1/access-requests/${requestId}`, undefined, false)
      if (!res.ok) return res
      const s = (res.value as { status?: string }).status
      return { ok: true, value: s === 'pending' || s === 'approved' || s === 'denied' || s === 'cancelled' ? s : 'expired' }
    },

    cancelAccess: async (computerId, requestId) => {
      const waiting = this.asking.get(computerId)
      if (waiting) waiting.cancelled = true
      const id = requestId ?? waiting?.requestId
      if (id && /^[A-Za-z0-9_-]{1,128}$/.test(id)) await this.d.cloud.request('DELETE', `v1/access-requests/${id}`, undefined, false)
    },
  }
}
