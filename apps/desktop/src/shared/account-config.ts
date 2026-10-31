/** The sign-in providers an account can use, and the Clerk strategy each one starts. */
export const SIGN_IN_PROVIDERS = ['apple', 'google', 'github'] as const
export type SignInProvider = (typeof SIGN_IN_PROVIDERS)[number]

const PROVIDER_NAMES: Record<SignInProvider, string> = { apple: 'Apple', google: 'Google', github: 'GitHub' }

export const providerName = (provider: SignInProvider) => PROVIDER_NAMES[provider]
export const providerStrategy = (provider: SignInProvider) => `oauth_${provider}`

/** Apple needs the production Clerk instance and a build that can hold Apple credentials (not the dev build). */
export const appleSignInAvailable = (environment: 'main' | 'dev', clerkPublishableKey: string | null) =>
  environment === 'main' && !!clerkPublishableKey?.startsWith('pk_live_')

export interface AccountConfig {
  clerkPublishableKey: string | null
  cloudURL: string | null
}

type Setting = string | null | undefined

/**
 * The Clerk key and cloud URL a build runs with. `KITH_*` in the environment wins over the build's account-config.json. Empty or malformed values mean "not configured".
 */
export function resolveAccountConfig(env: Record<string, string | undefined>, file: { clerkPublishableKey?: Setting; cloudURL?: Setting }): AccountConfig {
  const pick = (...values: Setting[]) => values.find((v) => v !== undefined && v !== null && v.trim() !== '')?.trim() ?? ''
  const key = pick(env.KITH_CLERK_PUBLISHABLE_KEY, file.clerkPublishableKey)
  const cloud = pick(env.KITH_CLOUD_URL, file.cloudURL)
  return {
    clerkPublishableKey: key.startsWith('pk_test_') || key.startsWith('pk_live_') ? key : null,
    cloudURL: /^https:\/\//.test(cloud) ? cloud.replace(/\/$/, '') : null,
  }
}
