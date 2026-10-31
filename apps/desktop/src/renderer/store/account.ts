import type { SignInProvider } from '../../shared/account-config'
import type { LinkedPhoto, ProfileImageSource } from '../../shared/profile-image'
import { Observable, useModel } from '../lib/observable'

export type { SignInProvider }

export interface AccountInfo {
  userId: string
  email: string | null
  avatarURL: string | null
  photos: LinkedPhoto[]
}

/**
 * The Kith account (Clerk, through the main process). An account never grants access to a
 * computer by itself: each one still approves this device (spec §4.2).
 */
export class AccountSession extends Observable {
  isBusy = false
  isReady = true
  isConfigured = false
  errorMessage: string | null = null
  user: AccountInfo | null = null
  cloudURL: string | null = null
  appleSignIn = false

  get isSignedIn() {
    return this.user !== null
  }

  async start() {
    const state = await window.kith.account.state()
    this.apply(state)
    window.kith.account.onChange((s) => this.apply(s))
  }

  private apply(s: Awaited<ReturnType<typeof window.kith.account.state>>) {
    this.isReady = s.ready
    this.isConfigured = s.configured
    this.isBusy = s.busy
    this.errorMessage = s.error
    this.user = s.user
    this.cloudURL = s.cloudURL
    this.appleSignIn = s.appleSignIn
    this.changed()
  }

  async signIn(provider: SignInProvider) {
    await window.kith.account.signIn(provider)
  }

  async signOut() {
    await window.kith.account.signOut()
  }

  /** Resolves to a sentence to show when the image couldn't be set, else null. Every avatar follows through the account state. */
  setProfileImage(source: ProfileImageSource) {
    return window.kith.account.setProfileImage(source)
  }

  sessionToken() {
    return window.kith.account.token()
  }
}

export const account = new AccountSession()

export function useAccount() {
  return useModel(account)
}
