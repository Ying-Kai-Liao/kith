import { paths } from '../../shared/cloud-contract.gen.ts'
import type { CloudFailure, CloudResult } from '../../shared/relay.ts'
import { signatureHeader, type Signer } from '../../shared/relay-core/crypto.ts'

/**
 * The Kith cloud's `/v1` API from the page: `fetch` with the Clerk session JWT as Bearer and, where a route asks for it,
 * `Kith-Sig` from the device key (design §3 steps 4-5, same canonical form as desktop's main/cloud.ts).
 *
 * Every request registers an abort controller. Sign-out first calls `halt()` (§4.2 step 1): in-flight requests are
 * aborted, nothing new starts, and a request still waiting on its token or signature stops before it reaches `fetch`,
 * so nothing signed before sign-out can land after the retire. `resume()` lifts that for the next sign-in.
 */

const signedOut = (): CloudResult<never> => ({ ok: false, error: { status: 401, code: 'signedOut', message: 'Sign in again to reach your account.' } })
const unreachable = (): CloudResult<never> => ({ ok: false, error: { status: 0, code: 'unreachable', message: "Can't reach the Kith cloud. Check your connection." } })

export interface CloudDeps {
  /** The cloud's origin, baked into the build; never taken from a link or from the cloud. */
  cloudURL: string
  /** A Clerk session JWT; throws when there is no live session. */
  token: () => Promise<string>
  /** The device key of the signed-in user, made on demand; null when signed out. */
  signer: () => Promise<Signer | null>
  fetch?: typeof fetch
  timeoutMs?: number
}

export class WebCloud {
  private halted = false
  private readonly inflight = new Set<AbortController>()

  private readonly d: CloudDeps

  constructor(deps: CloudDeps) {
    this.d = deps
  }

  /** Aborts everything in flight and refuses anything new until `resume`. */
  halt() {
    this.halted = true
    for (const c of this.inflight) c.abort()
    this.inflight.clear()
  }

  resume() {
    this.halted = false
  }

  get isHalted() {
    return this.halted
  }

  async request(method: string, path: string, body: unknown, signed: boolean): Promise<CloudResult<unknown>> {
    if (this.halted) return signedOut()
    const control = new AbortController()
    this.inflight.add(control)
    try {
      return await this.send(method, path, body, signed, control)
    } catch {
      return control.signal.aborted ? signedOut() : unreachable()
    } finally {
      this.inflight.delete(control)
    }
  }

  private async send(method: string, path: string, body: unknown, signed: boolean, control: AbortController): Promise<CloudResult<unknown>> {
    const url = new URL(path.replace(/^\//, ''), `${this.d.cloudURL.replace(/\/$/, '')}/`)
    const payload = body === undefined ? new Uint8Array() : new TextEncoder().encode(JSON.stringify(body))
    const headers: Record<string, string> = {}
    try {
      headers.Authorization = `Bearer ${await this.d.token()}`
    } catch (error) {
      return { ok: false, error: { status: 401, code: 'signedOut', message: error instanceof Error ? error.message : 'Sign in again.' } }
    }
    if (body !== undefined) headers['Content-Type'] = 'application/json'
    if (signed) {
      const signer = await this.d.signer()
      if (!signer) return signedOut()
      headers['Kith-Sig'] = await signatureHeader(signer, { method, authority: url.host, pathAndQuery: `${url.pathname || '/'}${url.search}`, body: payload })
    }
    // A sign-out that came while the token or the signature was being made: never let it reach the network.
    if (control.signal.aborted) return signedOut()
    return this.fetchJson(url, method, headers, body === undefined ? undefined : payload, control.signal)
  }

  /** `POST /v1/devices/retire` (§4.2 step 2): signed by the key alone, no login, and it ignores `halt` because it is the last word. */
  async retire(signer: Signer): Promise<CloudResult<unknown>> {
    const url = new URL(paths.retireDevice(), `${this.d.cloudURL.replace(/\/$/, '')}/`)
    try {
      const sig = await signatureHeader(signer, { method: 'POST', authority: url.host, pathAndQuery: url.pathname, body: new Uint8Array() })
      return await this.fetchJson(url, 'POST', { 'Kith-Sig': sig }, undefined, AbortSignal.timeout(this.d.timeoutMs ?? 8_000))
    } catch {
      return unreachable()
    }
  }

  private async fetchJson(url: URL, method: string, headers: Record<string, string>, payload: Uint8Array | undefined, outer: AbortSignal): Promise<CloudResult<unknown>> {
    const signal = AbortSignal.any([outer, AbortSignal.timeout(this.d.timeoutMs ?? 20_000)])
    const res = await (this.d.fetch ?? fetch)(url, { method, headers, body: payload as BodyInit | undefined, signal, credentials: 'omit', referrerPolicy: 'no-referrer' })
    const raw = await res.text()
    let json: Record<string, unknown> = {}
    try {
      json = raw ? (JSON.parse(raw) as Record<string, unknown>) : {}
    } catch {}
    if (!res.ok) {
      const e = (json.error ?? {}) as { code?: string; message?: string }
      const failure: CloudFailure = { status: res.status, code: e.code ?? `http${res.status}`, message: e.message ?? null }
      return { ok: false, error: failure }
    }
    return { ok: true, value: json }
  }
}
