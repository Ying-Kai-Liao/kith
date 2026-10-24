import { resolveAccountConfig } from '../../shared/account-config.ts'

/**
 * What this build talks to, fixed at build time by vite.web.config.ts (`KITH_WEB_CLOUD_URL`,
 * `KITH_WEB_CLERK_PUBLISHABLE_KEY`; the dev instance by default). It is never read from an invite link or from the cloud:
 * a link cannot point the page at another cloud (design §3, §7 item 4).
 */
declare const __KITH_WEB__: { cloudURL: string; clerkKey: string; version: string }

const resolved = resolveAccountConfig({ KITH_CLERK_PUBLISHABLE_KEY: __KITH_WEB__.clerkKey, KITH_CLOUD_URL: __KITH_WEB__.cloudURL }, {})

export const webConfig = {
  cloudURL: resolved.cloudURL ?? '',
  clerkPublishableKey: resolved.clerkPublishableKey ?? '',
  version: __KITH_WEB__.version,
  /** The scheme of the Kith app a person may already have; the dev build's. */
  appScheme: 'kith-dev',
}
