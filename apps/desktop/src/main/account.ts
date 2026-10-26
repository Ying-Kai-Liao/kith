import { EventEmitter } from 'node:events'
import { promises as fs } from 'node:fs'
import { join } from 'node:path'
import { fromRenderer } from './ipc-guard'
import { app, ipcMain, safeStorage, type BrowserWindow } from 'electron'
import { providerName, providerStrategy, resolveAccountConfig, type AccountConfig, type SignInProvider } from '../shared/account-config'
import type { AccountState } from '../shared/ipc'
import { linkedPhotos, profileImageProblem, type ProfileImageSource } from '../shared/profile-image'
import { authenticate } from './auth'
import { buildConfig, identity } from './environment'

/**
 * The Kith account through Clerk's Frontend API, the way ClerkKit drives it on the native
 * apps: a device token in the `Authorization` header (`_is_native=true`), OAuth in the system
 * browser returning to `kith://callback` (`kith-dev` in the dev build), the token kept encrypted with the OS
 * keychain. An account never grants access to a computer by itself (spec §4.2).
 */

const SCHEME = identity.scheme
const REDIRECT = `${SCHEME}://callback`

export const isMainEnvironment = () => identity.releaseUpdates

/** `KITH_CLERK_PUBLISHABLE_KEY` / `KITH_CLOUD_URL`, else the build's account config. */
const loadConfig = (): AccountConfig => resolveAccountConfig(process.env, buildConfig)

/** The Frontend API host is the publishable key's payload: `pk_test_<base64("host$")>`. */
function frontendAPI(key: string) {
  const host = Buffer.from(key.replace(/^pk_(test|live)_/, ''), 'base64').toString('utf8').replace(/\$$/, '')
  return `https://${host}`
}

interface ClerkUser {
  id: string
  image_url?: string
  primary_email_address_id?: string | null
  email_addresses?: { id: string; email_address: string }[]
  external_accounts?: { provider?: string; image_url?: string }[]
}

interface ClerkSession {
  id: string
  status: string
  expire_at?: number
  user?: ClerkUser
}

interface ClerkClient {
  sessions?: ClerkSession[]
  last_active_session_id?: string | null
}

interface Verification {
  status?: string
  external_verification_redirect_url?: string | null
  error?: { long_message?: string; message?: string } | null
}

interface SignInResource {
  id: string
  status: string
  created_session_id?: string | null
  first_factor_verification?: Verification | null
  second_factor_verification?: Verification | null
}

class ClerkError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message)
  }
}

export class AccountService extends EventEmitter {
  private config = loadConfig()
  private base = this.config.clerkPublishableKey ? frontendAPI(this.config.clerkPublishableKey) : null
  private deviceToken: string | null = null
  private client: ClerkClient | null = null
  private cachedToken: { sessionId: string; jwt: string; until: number } | null = null
  state: AccountState = {
    ready: !this.base,
    configured: !!this.base && !!this.config.cloudURL,
    busy: false,
    error: null,
    user: null,
    cloudURL: this.config.cloudURL,
  }

  private get storePath() {
    return join(app.getPath('userData'), 'account.bin')
  }

  register(windows: () => BrowserWindow[]) {
    ipcMain.handle('account:state', () => this.state)
    ipcMain.handle('account:signIn', (_e, provider: SignInProvider) => this.signIn(provider))
    ipcMain.handle('account:signOut', () => this.signOut())
    ipcMain.handle('account:setProfileImage', fromRenderer((_e, source: ProfileImageSource) => this.setProfileImage(source)))
    ipcMain.handle('account:token', fromRenderer(() => this.sessionToken()))
    this.on('change', () => {
      for (const w of windows()) w.webContents.send('account:change', this.state)
    })
    void this.restore()
  }

  private set(change: Partial<AccountState>) {
    this.state = { ...this.state, ...change }
    this.emit('change')
  }

  /** Clerk restores the persisted session first; fresh sign-in choices wait for it. */
  private async restore() {
    if (!this.base) return
    try {
      const sealed = await fs.readFile(this.storePath)
      const text = safeStorage.isEncryptionAvailable() ? safeStorage.decryptString(sealed) : sealed.toString('utf8')
      // A token from another Clerk application (an older Kith build, or a changed key) is no use here.
      const saved = JSON.parse(text) as { key?: string; token?: string }
      if (saved.key === this.config.clerkPublishableKey && saved.token) this.deviceToken = saved.token
      else await fs.rm(this.storePath, { force: true })
    } catch {}
    try {
      if (this.deviceToken) await this.refreshClient()
    } catch {
      // Offline at launch: the saved session still counts; the next call refreshes it.
    }
    this.set({ ready: true })
  }

  private async persist() {
    try {
      if (!this.deviceToken) return await fs.rm(this.storePath, { force: true })
      const text = JSON.stringify({ key: this.config.clerkPublishableKey, token: this.deviceToken })
      const data = safeStorage.isEncryptionAvailable() ? safeStorage.encryptString(text) : Buffer.from(text)
      await fs.mkdir(app.getPath('userData'), { recursive: true })
      await fs.writeFile(this.storePath, data, { mode: 0o600 })
    } catch {}
  }

  private async request<T>(method: 'GET' | 'POST' | 'DELETE', path: string, body?: Record<string, string | boolean> | FormData, query?: Record<string, string>): Promise<T> {
    if (!this.base) throw new ClerkError(0, "Sign-in isn't configured in this build.")
    const url = new URL(`${this.base}${path}`)
    url.searchParams.set('_is_native', 'true')
    for (const [k, v] of Object.entries(query ?? {})) url.searchParams.set(k, v)
    const headers: Record<string, string> = { 'x-device-type': process.platform === 'darwin' ? 'macos' : process.platform, 'x-app-version': app.getVersion(), 'x-bundle-id': identity.appId }
    if (this.deviceToken) headers.Authorization = this.deviceToken
    let payload: string | FormData | undefined
    if (body instanceof FormData) {
      // fetch sets the multipart Content-Type with its boundary.
      payload = body
    } else if (body) {
      headers['Content-Type'] = 'application/x-www-form-urlencoded'
      payload = new URLSearchParams(Object.entries(body).map(([k, v]) => [k, String(v)])).toString()
    }
    const res = await fetch(url, { method, headers, body: payload, signal: AbortSignal.timeout(20_000) })
    // A new device token comes back in `Authorization`; "Bearer" alone clears it.
    const auth = res.headers.get('authorization')?.trim()
    if (auth !== undefined) {
      this.deviceToken = !auth || auth.toLowerCase() === 'bearer' ? null : auth
      void this.persist()
    }
    const json = (await res.json().catch(() => ({}))) as { response?: T; client?: ClerkClient | null; errors?: { long_message?: string; message?: string }[] } & T
    if (!res.ok) throw new ClerkError(res.status, json.errors?.[0]?.long_message ?? json.errors?.[0]?.message ?? `Clerk error ${res.status}`)
    if ('client' in json) this.applyClient(json.client ?? null)
    return (json.response ?? json) as T
  }

  private applyClient(client: ClerkClient | null) {
    this.client = client
    const session = this.activeSession
    const user = session?.user
    const email = user?.email_addresses?.find((e) => e.id === user.primary_email_address_id)?.email_address ?? user?.email_addresses?.[0]?.email_address ?? null
    const image = user?.image_url && user.image_url.startsWith('https://') ? user.image_url : null
    this.set({ user: user ? { userId: user.id, email, avatarURL: image, photos: linkedPhotos(user.external_accounts) } : null })
  }

  /** Sets the profile image (POST /v1/me/profile_image, multipart `file`), then reloads the client so every avatar updates. Resolves to a sentence on failure. */
  async setProfileImage(source: ProfileImageSource): Promise<string | null> {
    if (!this.activeSession) return 'Sign in again to change your photo.'
    try {
      let bytes: ArrayBuffer
      let type: string
      if (source.kind === 'url') {
        // Only https, and only what the account itself listed: the renderer can't make main fetch arbitrary hosts.
        const allowed = this.state.user?.photos.some((p) => p.imageURL === source.url)
        if (!source.url.startsWith('https://') || !allowed) return "That photo isn't available."
        const res = await fetch(source.url, { signal: AbortSignal.timeout(20_000) })
        if (!res.ok) return "Couldn't download that photo."
        bytes = await res.arrayBuffer()
        type = (res.headers.get('content-type') ?? '').split(';')[0]!.trim()
        // Providers sometimes label a photo octet-stream; Clerk sniffs the bytes, so call it by the common case.
        if (!['image/png', 'image/jpeg', 'image/webp'].includes(type)) type = 'image/jpeg'
      } else {
        bytes = source.bytes
        type = source.type
      }
      const problem = profileImageProblem({ type, size: bytes.byteLength })
      if (problem) return problem
      const form = new FormData()
      form.append('file', new Blob([bytes], { type }), 'profile')
      const before = this.state.user?.avatarURL
      await this.request('POST', '/v1/me/profile_image', form)
      await this.refreshClient()
      // Clerk normally issues a new URL for a new image; if it didn't, the old cached picture would stay.
      const user = this.state.user
      if (user?.avatarURL && user.avatarURL === before) this.set({ user: { ...user, avatarURL: `${user.avatarURL}${user.avatarURL.includes('?') ? '&' : '?'}v=${Date.now()}` } })
      return null
    } catch (error) {
      return error instanceof ClerkError && error.status > 0 ? error.message : "Couldn't change your photo. Check your connection and try again."
    }
  }

  private get activeSession(): ClerkSession | null {
    const sessions = this.client?.sessions ?? []
    const active = sessions.find((s) => s.id === this.client?.last_active_session_id && s.status === 'active')
    return active ?? null
  }

  private async refreshClient() {
    const client = await this.request<ClerkClient | null>('GET', '/v1/client')
    this.applyClient(client)
  }

  async signIn(provider: SignInProvider) {
    if (this.state.busy) return
    if (!this.base) return this.set({ error: "Sign-in setup isn't finished yet. You can continue using local pairing." })
    const name = providerName(provider)
    this.set({ busy: true, error: null })
    try {
      const strategy = providerStrategy(provider)
      const created = await this.request<SignInResource>('POST', '/v1/client/sign_ins', { strategy, redirect_url: REDIRECT })
      const prepared = await this.request<SignInResource>('POST', `/v1/client/sign_ins/${created.id}/prepare_first_factor`, { strategy, redirect_url: REDIRECT })
      const redirect = prepared.first_factor_verification?.external_verification_redirect_url
      if (!redirect) throw new ClerkError(0, 'Redirect URL is missing. Unable to start the sign-in.')
      const callback = new URL(await authenticate(redirect, SCHEME))
      const nonce = callback.searchParams.get('rotating_token_nonce')
      const signIn = await this.request<SignInResource>('GET', `/v1/client/sign_ins/${created.id}`, undefined, nonce ? { rotating_token_nonce: nonce } : undefined)
      const transferable = signIn.first_factor_verification?.status === 'transferable' || signIn.second_factor_verification?.status === 'transferable'
      if (!nonce && transferable) {
        // A new account: Clerk's transfer flow turns the sign-in into a sign-up.
        const signUp = await this.request<{ status: string }>('POST', '/v1/client/sign_ups', { transfer: true })
        if (signUp.status !== 'complete') this.set({ error: `${name} sign-up needs additional account information. Your account is not signed in yet.` })
      } else if (signIn.status !== 'complete') {
        const error = signIn.first_factor_verification?.error
        if (error) throw new ClerkError(0, error.long_message ?? error.message ?? 'Sign-in failed.')
        this.set({ error: `${name} sign-in needs additional verification. Your account is not signed in yet.` })
      }
      await this.refreshClient()
    } catch (error) {
      const message = error instanceof Error ? error.message : ''
      if (!/timed out|Another sign-in/.test(message)) this.set({ error: "Couldn't sign in. Check your connection and try again." })
    } finally {
      this.set({ busy: false })
    }
  }

  async signOut() {
    const session = this.activeSession
    if (this.state.busy || !session) return
    this.set({ busy: true, error: null })
    try {
      await this.request('POST', `/v1/client/sessions/${session.id}/remove`)
      this.cachedToken = null
      await this.refreshClient()
    } catch {
      this.set({ error: "Couldn't sign out. Check your connection and try again." })
    } finally {
      this.set({ busy: false })
    }
  }

  /** Every account on this device, for starting over. */
  async signOutAll() {
    try {
      await this.request('DELETE', '/v1/client/sessions')
    } catch {}
    this.deviceToken = null
    this.cachedToken = null
    await this.persist()
    this.applyClient(null)
  }

  /** The signed-in session's JWT for the Kith cloud (cached for under a minute, like Clerk). */
  async sessionToken(): Promise<string> {
    const session = this.activeSession
    if (!session) throw new Error('Sign in again to reach your account.')
    if (this.cachedToken && this.cachedToken.sessionId === session.id && this.cachedToken.until > Date.now()) return this.cachedToken.jwt
    const res = await this.request<{ jwt?: string } | null>('POST', `/v1/client/sessions/${session.id}/tokens`)
    if (!res?.jwt) throw new Error('Sign in again to reach your account.')
    this.cachedToken = { sessionId: session.id, jwt: res.jwt, until: Date.now() + 50_000 }
    return res.jwt
  }
}
