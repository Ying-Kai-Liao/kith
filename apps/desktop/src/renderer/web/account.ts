import { providerName, providerStrategy, type SignInProvider } from '../../shared/account-config.ts'
import type { AccountState } from '../../shared/ipc.ts'
import { linkedPhotos, profileImageProblem, type ProfileImageSource } from '../../shared/profile-image.ts'

/**
 * The Kith account in the browser, through Clerk's JS SDK bundled from npm (`@clerk/clerk-js/no-rhc`, Clerk's build
 * with no remote-hosted code, so `script-src 'self'` holds). No Clerk UI: our own Google and GitHub buttons start the
 * OAuth redirect. An account never grants access to a computer by itself (spec §4.2); it only lets the page get a
 * session JWT for the cloud, and the cloud ties a web socket to that live session (design §4.5).
 */

/** The slice of the Clerk SDK this uses. */
export interface ClerkLike {
  load(options: Record<string, unknown>): Promise<void>
  readonly user?: {
    id: string
    imageUrl?: string
    primaryEmailAddress?: { emailAddress: string } | null
    emailAddresses?: { emailAddress: string }[]
    externalAccounts?: { provider?: string; imageUrl?: string }[]
    setProfileImage(params: { file: Blob | File | null }): Promise<unknown>
    reload?(): Promise<unknown>
  } | null
  readonly session?: {
    id: string
    status?: string
    getToken(options?: { skipCache?: boolean }): Promise<string | null>
    end(): Promise<unknown>
  } | null
  readonly client?: { signIn: { authenticateWithRedirect(params: Record<string, unknown>): Promise<void> } }
  addListener(cb: () => void): () => void
  handleRedirectCallback(params: Record<string, unknown>, navigate?: (to: string) => Promise<unknown>): Promise<unknown>
}

export interface AccountDeps {
  clerk: ClerkLike
  cloudURL: string
  /** Where Clerk sends the browser back to after the provider; the page itself. */
  origin: string
  /** Remembers across the redirect (this tab only) that a sign-in is on its way back. */
  marker: { get(): boolean; set(on: boolean): void }
}

export class WebAccount {
  state: AccountState
  private readonly listeners = new Set<(s: AccountState) => void>()

  constructor(private readonly d: AccountDeps) {
    this.state = { ready: false, configured: true, busy: false, error: null, user: null, cloudURL: d.cloudURL }
  }

  /** Loads Clerk, finishes a sign-in that just came back from the provider, and follows the session from then on. */
  async start(): Promise<void> {
    const { clerk } = this.d
    try {
      // Dev instances send usage telemetry to clerk-telemetry.com by default: off, so connect-src stays narrow.
      await clerk.load({ standardBrowser: true, telemetry: false, afterSignOutUrl: this.d.origin })
      if (this.d.marker.get()) {
        this.d.marker.set(false)
        await clerk.handleRedirectCallback({ signInFallbackRedirectUrl: this.d.origin, signUpFallbackRedirectUrl: this.d.origin }, async () => {})
        if (!clerk.user) this.set({ error: "Couldn't finish signing in. Try again." })
      }
    } catch {
      this.set({ error: "Couldn't reach the sign-in service. Check your connection and reload." })
    }
    clerk.addListener(() => this.apply())
    this.apply()
    this.set({ ready: true })
  }

  private apply() {
    const user = this.d.clerk.user
    const session = this.d.clerk.session
    const signedIn = !!user && session?.status !== 'ended' && session?.status !== 'removed' && session?.status !== 'revoked' && session?.status !== 'expired'
    const email = user?.primaryEmailAddress?.emailAddress ?? user?.emailAddresses?.[0]?.emailAddress ?? null
    const image = user?.imageUrl && user.imageUrl.startsWith('https://') ? user.imageUrl : null
    const photos = linkedPhotos(user?.externalAccounts?.map((a) => ({ provider: a.provider, image_url: a.imageUrl })))
    this.set({ user: signedIn && user ? { userId: user.id, email, avatarURL: image, photos } : null })
  }

  /** `user.setProfileImage({ file })`, then a reload so the new imageUrl reaches every avatar. Resolves to a sentence on failure. */
  async setProfileImage(source: ProfileImageSource): Promise<string | null> {
    const user = this.d.clerk.user
    if (!user) return 'Sign in again to change your photo.'
    try {
      let blob: Blob
      if (source.kind === 'url') {
        const allowed = this.state.user?.photos.some((p) => p.imageURL === source.url)
        if (!source.url.startsWith('https://') || !allowed) return "That photo isn't available."
        const res = await fetch(source.url)
        if (!res.ok) return "Couldn't download that photo."
        blob = await res.blob()
        if (!['image/png', 'image/jpeg', 'image/webp'].includes(blob.type)) blob = new Blob([blob], { type: 'image/jpeg' })
      } else {
        blob = new Blob([source.bytes], { type: source.type })
      }
      const problem = profileImageProblem(blob)
      if (problem) return problem
      await user.setProfileImage({ file: blob })
      await user.reload?.()
      this.apply()
      return null
    } catch (error) {
      const message = (error as { errors?: { longMessage?: string }[] })?.errors?.[0]?.longMessage
      return message ?? "Couldn't change your photo. Check your connection and try again."
    }
  }

  private set(change: Partial<AccountState>) {
    this.state = { ...this.state, ...change }
    for (const cb of this.listeners) cb(this.state)
  }

  onChange(cb: (s: AccountState) => void): () => void {
    this.listeners.add(cb)
    return () => void this.listeners.delete(cb)
  }

  /** The signed-in user and Clerk session, or null; the pair a device registration binds to (design §4.5). */
  session(): { userId: string; sid: string } | null {
    const user = this.state.user
    const session = this.d.clerk.session
    return user && session ? { userId: user.userId, sid: session.id } : null
  }

  async signIn(provider: SignInProvider): Promise<void> {
    if (this.state.busy) return
    const { clerk } = this.d
    if (provider === 'apple') return this.set({ error: 'Apple sign-in is not available in the browser yet.' })
    this.set({ busy: true, error: null })
    try {
      this.d.marker.set(true)
      // Leaves the page for the provider and returns to it; the invite link was sealed into IndexedDB before this.
      await clerk.client?.signIn.authenticateWithRedirect({
        strategy: providerStrategy(provider),
        redirectUrl: this.d.origin,
        redirectUrlComplete: this.d.origin,
      })
    } catch {
      this.d.marker.set(false)
      this.set({ busy: false, error: `Couldn't start ${providerName(provider)} sign-in. Check your connection and try again.` })
    }
  }

  /** The session JWT for the cloud. Throws when there is no live session. */
  async token(): Promise<string> {
    const jwt = await this.d.clerk.session?.getToken()
    if (!jwt) throw new Error('Sign in again to reach your account.')
    return jwt
  }

  /** A JWT minted now for a socket upgrade (the old one may be about to expire). Null: definitely no session. */
  async freshToken(): Promise<string | null> {
    const session = this.d.clerk.session
    if (!session) return null
    return session.getToken({ skipCache: true })
  }

  /** Ends the Clerk session only (§4.2 step 4, and "Sign in again"): no key or token is touched. */
  async endSession(): Promise<void> {
    try {
      await this.d.clerk.session?.end()
    } catch {}
    this.apply()
  }
}
