import { paths } from '../cloud-contract.gen.ts'
import type { RelayState } from '../relay.ts'
import { FrameOpener, FrameSealer, Handshake, TooLarge, computerIdOf, fromText, signatureHeader, text as utf8, unb64url, type Signer } from './crypto.ts'

/**
 * One device-to-Mac channel over the Kith cloud relay (docs/reference/remote-relay.md §7): the signed upgrade, the
 * hello/welcome handshake pinned to the computer's sign key, encrypted frames carrying inner RPC (§6.5), and the
 * reconnect loop. The iOS client (ChannelTransport) is the other implementation of this; the two behave alike.
 * Shared by the desktop main process and the web client: no node, electron or DOM imports. The socket is injected
 * (`Dial`), so each side keeps its own way of authenticating the upgrade, and node's test runner loads it with a fake one.
 */

/** Something went wrong reaching or talking to the Mac. `status` 0 means the link, not the request: retrying can work. */
export class RelayError extends Error {
  readonly status: number
  readonly code: string
  constructor(status: number, message: string, code = 'error') {
    super(message)
    this.status = status
    this.code = code
  }
}

const UNREACHABLE = "Can't reach your computer. Is it on and connected to the internet?"
const OFFLINE = 'That computer is offline.'
const REMOVED = "Your access to this computer was removed."
const IDENTITY_CHANGED = "This computer's identity changed, so Kith won't connect to it. Remove it and add it again if you expected that."
const UPGRADE = 'This computer needs a newer Kith. Update it, then try again.'
const UPDATE_APP = 'This version of Kith is too old to connect. Update Kith, then try again.'
const UNKNOWN_COMPUTER = "This computer isn't on your account."
const SIGNED_OUT = 'Sign in again to reach your computers.'

const failedStatus = (code: string) => (code === 'upgrade' ? 426 : code === 'unknownComputer' ? 404 : code === 'signedOut' ? 401 : 0)

const unreachable = () => new RelayError(0, UNREACHABLE, 'unreachable')

export type LinkState = RelayState

/** The WebSocket seam: the default wraps the global one, tests pass a fake. */
export interface Socket {
  send(text: string): void
  close(code: number): void
  onopen: (() => void) | null
  onmessage: ((text: string) => void) | null
  onclose: ((code: number) => void) | null
}

/** What authenticates the upgrade: the `Kith-Sig` header value, which each dialer carries its own way. */
export interface DialAuth {
  sig: string
}

/** Opens the socket. Async so a browser dialer can fetch a fresh session token first. */
export type Dial = (url: string, auth: DialAuth) => Socket | Promise<Socket>

/** Repeats a refused upgrade as plain HTTP to learn the status (null: no answer). Only desktop can; a browser has no probe. */
export type Probe = (url: string, auth: DialAuth) => Promise<number | null>

type Outcome =
  | { kind: 'again' }
  | { kind: 'retry'; message: string }
  | { kind: 'wait'; state: LinkState }
  | { kind: 'stop'; state: LinkState }

/** 1 s doubling to 30 s with a little jitter, so Macs coming back don't all knock at once. */
export class Backoff {
  private attempt = 0
  private readonly random: () => number
  constructor(random: () => number = Math.random) {
    this.random = random
  }

  next(): number {
    const base = Math.min(30_000, 1000 * 2 ** this.attempt)
    this.attempt += 1
    return Math.round(base * (0.8 + 0.4 * this.random()))
  }
}

export interface LinkOptions {
  /** `https://cloud.example` (the account's cloud). */
  cloudURL: string
  computerId: string
  /** The computer's Ed25519 sign key, b64url: what the welcome must be signed by. */
  hostSignKey: string
  signer: Signer
  dial: Dial
  probe?: Probe
  random?: () => number
  /** Ms between a failed attempt and the next; default `Backoff`. */
  delay?: () => number
}

interface Pending {
  resolve: (value: unknown) => void
  reject: (error: Error) => void
  timer: ReturnType<typeof setTimeout>
}

interface StreamSub {
  onEvent: (line: string) => void
  onEnd: (error: RelayError | null) => void
}

interface Active {
  socket: Socket
  generation: number
  handshake: Handshake | null
  sealer: FrameSealer | null
  opener: FrameOpener | null
  lastReceived: number
}

const PING = '{"t":"ping"}'

export class RelayLink {
  private readonly o: LinkOptions
  private readonly hostPub: Uint8Array | null
  private _state: LinkState = { kind: 'connecting' }
  private readonly listeners = new Set<(s: LinkState) => void>()
  private waiters = new Set<() => void>()
  private active: Active | null = null
  private generation = 0
  private nextId = 0
  private readonly calls = new Map<number, Pending>()
  private readonly streams = new Map<number, StreamSub>()
  private closed = false
  private running = false
  private restartRequested = false
  private refusals = 0
  private backoff: Backoff
  private heartbeat: ReturnType<typeof setInterval> | null = null

  constructor(options: LinkOptions) {
    this.o = options
    this.backoff = new Backoff(options.random)
    const pub = unb64url(options.hostSignKey)
    this.hostPub = pub && pub.length === 32 && computerIdOf(pub) === options.computerId ? pub : null
    // A key that doesn't hash to the computer's id is never dialled, with nothing to fall back to.
    if (!this.hostPub) this._state = { kind: 'failed', message: "This computer's key doesn't match its ID.", code: 'badSignKey', final: true }
  }

  get state() {
    return this._state
  }

  onState(cb: (s: LinkState) => void): () => void {
    this.listeners.add(cb)
    return () => this.listeners.delete(cb)
  }

  /** Starts connecting (once). Calls and streams start it themselves. */
  start() {
    if (this.closed || this.running || !this.hostPub) return
    if (this._state.kind === 'unauthorized' || (this._state.kind === 'failed' && this._state.final)) return
    this.running = true
    void this.run().finally(() => {
      this.running = false
    })
  }

  /** After a stop (access removed, wrong key, needs an update): try again from a clean slate, e.g. once access is granted. */
  restart() {
    if (this.closed || !this.hostPub) return
    this.refusals = 0
    this.backoff = new Backoff(this.o.random)
    if (this.running) {
      this.restartRequested = true
      this.active?.socket.close(1000)
      this.wake()
      return
    }
    this.setState({ kind: 'connecting' })
    this.start()
  }

  close() {
    this.closed = true
    this.stopHeartbeat()
    this.active?.socket.close(1000)
    this.dropChannel(unreachable())
    this.wake()
  }

  // ---- calls and streams (§6.5) ----

  async call(method: string, body: unknown, timeoutMs: number): Promise<unknown> {
    await this.ready(timeoutMs)
    const id = this.newId()
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => this.abandon(id, unreachable()), timeoutMs)
      this.calls.set(id, { resolve, reject, timer })
      try {
        this.sendInner({ id, m: method, b: body ?? null })
      } catch (e) {
        this.abandon(id, e instanceof RelayError ? e : unreachable(), false)
      }
    })
  }

  /** Events of `sub` ("events" with `{since, client}`): each element is one event's JSON. Returns the cancel function. */
  subscribe(sub: string, body: unknown, onEvent: (line: string) => void, onEnd: (error: RelayError | null) => void): () => void {
    let id: number | null = null
    let cancelled = false
    void (async () => {
      try {
        await this.ready(10_000)
        if (cancelled) return
        id = this.newId()
        this.streams.set(id, { onEvent, onEnd })
        this.sendInner({ id, sub, b: body ?? null })
      } catch (e) {
        if (!cancelled) onEnd(e instanceof RelayError ? e : unreachable())
      }
    })()
    return () => {
      cancelled = true
      if (id !== null && this.streams.delete(id)) {
        try {
          this.sendInner({ id, cancel: true })
        } catch {}
      }
    }
  }

  private newId() {
    this.nextId = this.nextId >= 0xffffffff ? 1 : this.nextId + 1
    return this.nextId
  }

  /** Timed out or cancelled: the host drops the answer (a mutation already started still runs). */
  private abandon(id: number, error: RelayError, tell = true) {
    const call = this.calls.get(id)
    if (!call) return
    this.calls.delete(id)
    clearTimeout(call.timer)
    call.reject(error)
    if (tell) {
      try {
        this.sendInner({ id, cancel: true })
      } catch {}
    }
  }

  /** Waits (bounded) while connecting; throws what the link state means otherwise. */
  private async ready(limitMs: number) {
    this.start()
    const deadline = Date.now() + Math.max(limitMs, 0)
    for (;;) {
      if (this.closed) throw unreachable()
      const s = this._state
      switch (s.kind) {
        case 'ready':
          if (this.active?.sealer) return
          break
        case 'connecting':
          break
        case 'hostOffline':
          throw new RelayError(0, OFFLINE, 'hostOffline')
        case 'unauthorized':
          throw new RelayError(401, s.message, s.retrying ? 'leaseExpired' : 'accessRemoved')
        case 'failed':
          // A retrying failure is waited out like connecting; a final one is the answer.
          if (s.final) throw new RelayError(failedStatus(s.code), s.message, s.code)
          break
      }
      const left = deadline - Date.now()
      if (left <= 0) throw s.kind === 'failed' ? new RelayError(0, s.message, s.code) : unreachable()
      await this.stateChange(left)
    }
  }

  private stateChange(ms: number) {
    return new Promise<void>((resolve) => {
      const done = () => {
        clearTimeout(timer)
        this.waiters.delete(done)
        resolve()
      }
      const timer = setTimeout(done, ms)
      this.waiters.add(done)
    })
  }

  private wake() {
    for (const w of [...this.waiters]) w()
  }

  private setState(s: LinkState) {
    this._state = s
    for (const l of this.listeners) l(s)
    this.wake()
  }

  // ---- the connection loop (§7.5, §7.6) ----

  private async run() {
    let backoff = this.backoff
    while (!this.closed) {
      if (this._state.kind === 'ready' || this._state.kind === 'failed') this.setState({ kind: 'connecting' })
      const started = Date.now()
      const outcome = await this.attempt()
      if (this.closed) return
      if (this.restartRequested) {
        this.restartRequested = false
        backoff = this.backoff
        this.setState({ kind: 'connecting' })
        continue
      }
      if (outcome.kind === 'again') {
        backoff = this.backoff = new Backoff(this.o.random)
        continue
      }
      if (outcome.kind === 'stop') {
        this.setState(outcome.state)
        this.dropChannel(this.errorFor(outcome.state))
        return
      }
      // Stable for a minute: the next failure starts the backoff over.
      if (Date.now() - started > 60_000) backoff = this.backoff = new Backoff(this.o.random)
      if (outcome.kind === 'wait') this.setState(outcome.state)
      // The relay said the computer is off: keep saying so rather than "can't reach".
      else if (this._state.kind !== 'hostOffline') this.setState({ kind: 'failed', message: outcome.message, code: 'unreachable', final: false })
      const deadline = Date.now() + (this.o.delay ? this.o.delay() : backoff.next())
      while (!this.closed && !this.restartRequested && Date.now() < deadline) await this.stateChange(deadline - Date.now())
      this.restartRequested = false
    }
  }

  private errorFor(s: LinkState): RelayError {
    if (s.kind === 'unauthorized') return new RelayError(401, s.message, s.retrying ? 'leaseExpired' : 'accessRemoved')
    if (s.kind === 'failed') return new RelayError(failedStatus(s.code), s.message, s.code)
    return unreachable()
  }

  /** `GET {cloud}/v1/relay/device/{computerId}?v=1`, signed with the device key. */
  private async upgrade(): Promise<{ url: string; auth: DialAuth }> {
    const base = new URL(this.o.cloudURL)
    const pathAndQuery = `/${paths.relayDevice(this.o.computerId)}`
    const sig = await signatureHeader(this.o.signer, { method: 'GET', authority: base.host, pathAndQuery })
    const ws = new URL(pathAndQuery, base)
    ws.protocol = base.protocol === 'http:' ? 'ws:' : 'wss:'
    return { url: ws.toString(), auth: { sig } }
  }

  private async attempt(): Promise<Outcome> {
    let socket: Socket
    try {
      const { url, auth } = await this.upgrade()
      socket = await this.o.dial(url, auth)
    } catch (e) {
      // No live session to carry the upgrade (web): stop until the page signs in again and restarts the link.
      if (e instanceof RelayError && e.code === 'signedOut') return { kind: 'stop', state: { kind: 'failed', message: e.message, code: 'signedOut', final: true } }
      return { kind: 'retry', message: UNREACHABLE }
    }
    return this.serve(socket)
  }

  /** Runs one socket until it ends. */
  private serve(socket: Socket): Promise<Outcome> {
    const generation = ++this.generation
    const active: Active = { socket, generation, handshake: null, sealer: null, opener: null, lastReceived: Date.now() }
    this.active = active
    let opened = false
    return new Promise<Outcome>((resolve) => {
      let finished = false
      const finish = (outcome: Outcome) => {
        if (finished) return
        finished = true
        this.stopHeartbeat()
        if (this.active === active) this.active = null
        this.dropChannel(unreachable())
        resolve(outcome)
      }
      socket.onopen = () => {
        opened = true
        this.startHeartbeat(active)
      }
      socket.onmessage = (text) => {
        if (finished || this.active !== active) return
        active.lastReceived = Date.now()
        const outcome = this.handle(text, active)
        if (outcome) {
          socket.close(1000)
          finish(outcome)
        }
      }
      socket.onclose = (code) => {
        if (opened) return finish(this.outcomeForClose(code))
        void this.outcomeForRefusal().then(finish)
      }
      if (this.closed || this.restartRequested) socket.close(1000)
    })
  }

  /** The upgrade never completed. undici doesn't say why, so a computer the cloud keeps refusing is retried quietly and slowly. */
  private async outcomeForRefusal(): Promise<Outcome> {
    const status = await this.probeRefusal()
    if (status === 426) return { kind: 'stop', state: { kind: 'failed', message: UPDATE_APP, code: 'upgrade', final: true } }
    if (status === 404) return { kind: 'stop', state: { kind: 'failed', message: UNKNOWN_COMPUTER, code: 'unknownComputer', final: true } }
    this.refusals += 1
    return { kind: 'retry', message: this.refusals > 3 ? `${UNREACHABLE} If this keeps happening, your access may have been removed.` : UNREACHABLE }
  }

  /** Asks the relay again, in the open, why it refused; a failed probe changes nothing. */
  private async probeRefusal(): Promise<number | null> {
    try {
      if (!this.o.probe) return null
      const { url, auth } = await this.upgrade()
      return await this.o.probe(url, auth)
    } catch {
      return null
    }
  }

  /** The one place a close code becomes a link outcome. The 44xx codes are what a browser gets for a refused upgrade (design §4.6). */
  private outcomeForClose(code: number): Outcome {
    switch (code) {
      case 4001:
      case 4003:
      case 4403:
        return { kind: 'stop', state: { kind: 'unauthorized', message: REMOVED } }
      case 4400:
        return { kind: 'stop', state: { kind: 'failed', message: UPGRADE, code: 'upgrade', final: true } }
      case 4426:
        return { kind: 'stop', state: { kind: 'failed', message: UPDATE_APP, code: 'upgrade', final: true } }
      case 4404:
        return { kind: 'stop', state: { kind: 'failed', message: UNKNOWN_COMPUTER, code: 'unknownComputer', final: true } }
      case 4401:
        return { kind: 'stop', state: { kind: 'failed', message: SIGNED_OUT, code: 'signedOut', final: true } }
      case 4100:
      case 4011:
        return { kind: 'again' }
      default:
        return { kind: 'retry', message: UNREACHABLE }
    }
  }

  /** What a `reject` means, or null when a plain retry may succeed (rate limits, clock skew). */
  private outcomeForReject(code: string | undefined, message: string | undefined): Outcome | null {
    switch (code) {
      case 'unauthorized':
      case 'revoked':
        return { kind: 'stop', state: { kind: 'unauthorized', message: message || REMOVED } }
      case 'leaseExpired':
        // The computer couldn't confirm the account's approval lately; it may again.
        return { kind: 'wait', state: { kind: 'unauthorized', message: message || "Your computer couldn't confirm this device's access. Retrying.", retrying: true } }
      case 'unsupportedVersion':
        return { kind: 'stop', state: { kind: 'failed', message: UPGRADE, code: 'upgrade', final: true } }
      default:
        return null
    }
  }

  private startHeartbeat(active: Active) {
    this.stopHeartbeat()
    // The relay answers a text ping with a pong and closes a socket silent for 75 s (§7.5).
    this.heartbeat = setInterval(() => {
      if (this.active !== active) return this.stopHeartbeat()
      if (Date.now() - active.lastReceived > 75_000) return active.socket.close(1000)
      try {
        active.socket.send(PING)
      } catch {}
    }, 30_000)
    this.heartbeat.unref?.()
  }

  private stopHeartbeat() {
    if (this.heartbeat) clearInterval(this.heartbeat)
    this.heartbeat = null
  }

  // ---- incoming ----

  private handle(text: string, active: Active): Outcome | null {
    let wire: Record<string, unknown>
    try {
      wire = JSON.parse(text) as Record<string, unknown>
    } catch {
      return null
    }
    if (!wire || typeof wire.t !== 'string') return null
    switch (wire.t) {
      case 'presence':
        if (wire.online === true) {
          // The host (re)joined: any previous channel is gone; start a fresh handshake.
          this.dropChannel(unreachable())
          return this.startHandshake(active)
        }
        active.handshake = active.sealer = active.opener = null
        this.dropChannel(new RelayError(0, OFFLINE, 'hostOffline'))
        this.setState({ kind: 'hostOffline', lastSeenAt: typeof wire.lastSeenAt === 'number' ? wire.lastSeenAt : null })
        return null
      case 'welcome': {
        const hs = active.handshake
        const ek = typeof wire.ek === 'string' ? unb64url(wire.ek) : null
        const sig = typeof wire.sig === 'string' ? unb64url(wire.sig) : null
        if (!hs || !ek || !sig) return { kind: 'retry', message: UNREACHABLE }
        active.handshake = null
        try {
          const keys = hs.finish(ek, sig)
          active.sealer = new FrameSealer(keys.d2h)
          active.opener = new FrameOpener(keys.h2d)
        } catch {
          // The pinned key didn't sign it: never fall back to trusting whatever answered.
          return { kind: 'stop', state: { kind: 'failed', message: IDENTITY_CHANGED, code: 'identityChanged', final: true } }
        }
        this.refusals = 0
        this.setState({ kind: 'ready' })
        return null
      }
      case 'reject': {
        const code = typeof wire.code === 'string' ? wire.code : undefined
        const message = typeof wire.message === 'string' ? wire.message : undefined
        return this.outcomeForReject(code, message) ?? { kind: 'retry', message: message || UNREACHABLE }
      }
      case 'f': {
        const c = wire.c
        const d = typeof wire.d === 'string' ? unb64url(wire.d) : null
        if (!active.opener || typeof c !== 'number' || !Number.isInteger(c) || c < 0 || !d) {
          active.socket.close(4002)
          return { kind: 'retry', message: 'Protocol error' }
        }
        try {
          const message = active.opener.open(c, d)
          if (message) this.handleInner(message)
        } catch (e) {
          active.socket.close(e instanceof TooLarge ? 4013 : 4002)
          return { kind: 'retry', message: e instanceof TooLarge ? 'Message too large' : 'Protocol error' }
        }
        return null
      }
      default:
        return null
    }
  }

  private startHandshake(active: Active): Outcome | null {
    if (!this.hostPub) return { kind: 'stop', state: this._state }
    const hs = new Handshake(this.o.signer, this.hostPub)
    active.handshake = hs
    active.sealer = active.opener = null
    if (this._state.kind !== 'connecting') this.setState({ kind: 'connecting' })
    // Signing is async. A newer presence replaces `active.handshake` meanwhile, and then this hello is stale and dropped,
    // so only the latest handshake's hello is ever sent on a socket.
    void hs.hello().then(
      (hello) => {
        if (this.active === active && active.handshake === hs) active.socket.send(JSON.stringify(hello))
      },
      () => {
        if (this.active === active && active.handshake === hs) active.socket.close(1000)
      },
    )
    // No welcome within 10 s: give up on this socket.
    const timer = setTimeout(() => {
      if (this.active === active && active.handshake === hs) active.socket.close(1000)
    }, 10_000)
    timer.unref?.()
    return null
  }

  /** Inner RPC (§6.5): `ok`/`err` answer calls, `ev`/`end`/`err` feed streams. */
  private handleInner(message: Uint8Array) {
    let obj: Record<string, unknown>
    try {
      obj = JSON.parse(fromText(message)) as Record<string, unknown>
    } catch {
      return
    }
    const id = obj?.id
    if (typeof id !== 'number') return
    if ('ev' in obj) {
      this.streams.get(id)?.onEvent(JSON.stringify(obj.ev))
    } else if (obj.end === true) {
      const s = this.streams.get(id)
      this.streams.delete(id)
      s?.onEnd(null)
    } else if (obj.err && typeof obj.err === 'object') {
      const err = obj.err as { status?: unknown; message?: unknown }
      // The host's own words and status, unchanged (e.g. an unproven member's refusal).
      const error = new RelayError(typeof err.status === 'number' ? err.status : 500, typeof err.message === 'string' ? err.message : `Host error ${err.status}`, 'host')
      const call = this.calls.get(id)
      if (call) {
        this.calls.delete(id)
        clearTimeout(call.timer)
        call.reject(error)
      } else {
        const s = this.streams.get(id)
        this.streams.delete(id)
        s?.onEnd(error)
      }
    } else if ('ok' in obj) {
      const call = this.calls.get(id)
      if (!call) return
      this.calls.delete(id)
      clearTimeout(call.timer)
      call.resolve(obj.ok)
    }
  }

  private sendInner(message: unknown) {
    const active = this.active
    if (!active?.sealer) throw unreachable()
    const frames = active.sealer.seal(utf8(JSON.stringify(message)))
    // One synchronous burst, so a message's chunks never interleave with another's.
    for (const f of frames) active.socket.send(JSON.stringify({ t: 'f', c: f.c, d: f.d }))
    // Counters must never wrap: replace the channel (§3.5).
    if (active.sealer.exhausted) {
      this.restartRequested = true
      active.socket.close(4011)
    }
  }

  /** Ends everything riding on the channel; the stores resubscribe from their own revision. */
  private dropChannel(error: RelayError) {
    const calls = [...this.calls.values()]
    const streams = [...this.streams.values()]
    this.calls.clear()
    this.streams.clear()
    for (const c of calls) {
      clearTimeout(c.timer)
      c.reject(error)
    }
    for (const s of streams) s.onEnd(error)
  }
}
