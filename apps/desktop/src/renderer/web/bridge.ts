import './lockdown.ts'
import { Clerk } from '@clerk/clerk-js/no-rhc'
import type { AccountState, CallError, HostSnapshot, KithBridge, UpdateState } from '../../shared/ipc.ts'
import { isInviteProof } from '../../shared/invite.ts'
import { isSafeExternalURL } from '../../shared/navigation.ts'
import { paths, type Requests } from '../../shared/cloud-contract.gen.ts'
import type { CloudFailure } from '../../shared/relay.ts'
import { WebAccount, type ClerkLike } from './account.ts'
import { WebCloud } from './cloud.ts'
import { webConfig } from './config.ts'
import { detectBrowser, type Unsupported } from './detect.ts'
import { appInviteLink, takeInviteFromHash } from './hash.ts'
import { indexedDbKv, KeyStore, requestPersistence } from './keystore.ts'
import { WebRelay } from './relay.ts'
import { refusingKeys } from './refusal.ts'
import { provideWebSession } from './session.ts'
import { signOutInOrder } from './signout.ts'

// `window.kith` for the browser (design §2): the real renderer on the member-only bridge. Account through Clerk's bundled
// JS SDK, the cloud over signed fetch, the relay over the shared core with the browser dialer, invites in IndexedDB.
// Everything owner- or desktop-only (loopback host, ssh, island, tray, updates, calls, screen, speech, files) is unavailable
// as in the website demo. Imported before the renderer so module-level reads see it.

const none = () => () => {}
const NOT_HERE = 'This is not available in the browser.'
const unavailable = () => Promise.reject(new Error(NOT_HERE))

// The invite leaves the address bar before anything else runs (§3 step 1).
const hashLink = takeInviteFromHash(location, history)

const keys = new KeyStore(indexedDbKv())
const ssoMarker = {
  get: () => {
    try {
      return sessionStorage.getItem('kith-web-sso') === '1'
    } catch {
      return false
    }
  },
  set: (on: boolean) => {
    try {
      if (on) sessionStorage.setItem('kith-web-sso', '1')
      else sessionStorage.removeItem('kith-web-sso')
    } catch {}
  },
}

// Clerk sends the browser back to the page itself (kithbot.app/join/), not the site's root, which is the landing page.
const origin = new URL(import.meta.env.BASE_URL, location.origin).href
const account = new WebAccount({
  clerk: new Clerk(webConfig.clerkPublishableKey) as unknown as ClerkLike,
  cloudURL: webConfig.cloudURL,
  origin,
  marker: ssoMarker,
})

const currentUser = () => account.state.user?.userId ?? null

/** Set when the cloud refused this user's key: no new key is made in this page, only after an explicit reload. */
const refusing = refusingKeys(keys, currentUser)

const cloud = new WebCloud({
  cloudURL: webConfig.cloudURL,
  token: () => account.token(),
  signer: refusing.cloudSigner,
})

const relay = new WebRelay({
  cloud,
  cloudURL: webConfig.cloudURL,
  session: () => account.session(),
  jwt: () => account.freshToken(),
  signer: refusing.relaySigner,
  keyRefused: refusing.refused,
  deviceName: 'Kith web',
})

/** Browser support is decided first; nothing touches Clerk, IndexedDB or the network in a browser that fails it. */
let support: Unsupported | null = null
const ready: Promise<void> = (async () => {
  support = await detectBrowser()
  if (support) return
  // The link is sealed before sign-in: the redirect to the provider leaves the page, and a reload must resume the join.
  if (hashLink) await keys.savePending(hashLink).catch(() => {})
  void requestPersistence()
  await account.start()
})()

provideWebSession({
  ready: () => ready,
  support: async () => (await ready, support),
  pendingLink: async () => {
    if (support) return null
    const link = await keys.pending(currentUser()).catch(() => null)
    return link ? { computerId: link.computerId, appLink: appInviteLink(webConfig.appScheme, link) } : null
  },
  discardPending: () => keys.clearPending().catch(() => {}),
  signInAgain: () => account.endSession(),
})

const failure = (status: number, code: string, message: string) => ({ ok: false as const, error: { status, code, message } satisfies CloudFailure })

async function signOut() {
  const userId = currentUser()
  try {
    await signOutInOrder({
      halt: () => cloud.halt(),
      closeLinks: () => relay.closeAll(),
      signer: async () => (userId ? keys.identity(userId, false) : null),
      retire: (signer) => cloud.retire(signer),
      wipe: async () => {
        if (userId) await keys.wipe(userId)
        else await keys.clearPending()
        // The mirror cache and drafts are this person's chats in plain text: gone with the sign-out.
        try {
          localStorage.clear()
        } catch {}
      },
      endSession: () => account.endSession(),
    })
  } finally {
    cloud.resume()
    // A clean page: nothing of the signed-out person stays in memory.
    location.replace(location.pathname)
  }
}

/**
 * The composer's + in a browser: the page's own file input, opened in the click itself (Safari allows no other time).
 * Cancelling resolves with nothing. The bytes stay in the page until the composer uploads them to the chat.
 */
function pickFiles(): Promise<{ name: string; data: Uint8Array }[]> {
  return new Promise((resolve) => {
    const input = document.createElement('input')
    input.type = 'file'
    input.multiple = true
    input.addEventListener('cancel', () => resolve([]))
    input.addEventListener('change', () => {
      const picked = [...(input.files ?? [])]
      void Promise.all(picked.map(async (f) => ({ name: f.name, data: new Uint8Array(await f.arrayBuffer()) })))
        .then(resolve, () => resolve([]))
    })
    input.click()
  })
}

const snapshot: HostSnapshot = { state: { kind: 'notInstalled' }, local: null, dev: false, logPath: '' }
const updates: UpdateState = {
  supported: false, checking: false, availableVersion: null, downloadPercent: null, staged: false, installing: false,
  autoCheck: false, autoDownload: false, lastCheck: null, error: null, waitingForApp: null,
}

const bridge: KithBridge = {
  platform: 'web',
  appName: 'Kith',
  appScheme: webConfig.appScheme,
  hostPort: 0,
  appVersion: webConfig.version,
  computerName: 'This browser',
  debugOpen: null,
  host: {
    snapshot: async () => snapshot,
    onChange: none,
    install() {},
    restart() {},
    call: () => Promise.reject({ status: 0, message: NOT_HERE } satisfies CallError),
    stream: (_url, _token, _onData, onEnd) => {
      queueMicrotask(() => onEnd({ status: 0, message: NOT_HERE }))
      return () => {}
    },
  },
  account: {
    state: async (): Promise<AccountState> => (await ready, support ? { ready: true, configured: false, busy: false, error: null, user: null, cloudURL: null } : account.state),
    onChange: (cb) => account.onChange(cb),
    signIn: (provider) => account.signIn(provider),
    signOut,
    setProfileImage: (source) => account.setProfileImage(source),
    token: () => account.token(),
  },
  speech: { available: false, locales: () => [], start() {}, stop() {}, onEvent: none },
  ssh: {
    state: async () => ({ profiles: [], status: {}, attachments: [] }),
    onChange: none,
    save: async () => NOT_HERE,
    remove() {},
    connect() {},
    disconnect() {},
    trustHostKey() {},
    chooseKey: async () => null,
  },
  island: {
    supported: false,
    enabled: async () => false,
    setEnabled: async () => false,
    setSummary() {},
    state: async () => ({ screen: { notch: null }, summary: { bots: [], permissions: [], usage: [] } }),
    onSummary: none,
    onScreen: none,
    onHover: none,
    setShape() {},
    setFocusable() {},
    setKeys() {},
    onKey: none,
    respond() {},
    send() {},
    openBot() {},
    newBot() {},
    openApp() {},
  },
  incomingCall: { show: async () => false, close() {}, onAnswer: none, current: async () => null, onCall: none, respond() {} },
  cloud: {
    request: async (method, path, body, signed) => {
      await ready
      // Anything that registers this device is the one registration of this tab (desktop's CloudModel does it on sign-in).
      if (method === 'POST' && path.replace(/^\//, '') === paths.registerDevice()) {
        const res = await relay.ensureDevice()
        return res.ok ? { ok: true, value: {} } : res
      }
      return cloud.request(method, path, body, signed)
    },
    // The cloud gets h1 = sha256(t) and nothing else of the link; the shape is checked so a slip cannot put `t` on the wire.
    acceptInvite: async (inviteId, h1) => {
      if (!/^[A-Za-z0-9_-]{1,128}$/.test(inviteId) || !isInviteProof(h1)) return failure(400, 'badInvite', 'This invite link looks wrong. Ask for a new one.')
      await ready
      const device = await relay.ensureDevice()
      if (!device.ok) return device
      return cloud.request('POST', paths.acceptInvite(inviteId), { token: h1 } satisfies Requests['acceptInvite'], false)
    },
  },
  relay: relay.bridge,
  invites: {
    takePending: async () => {
      await ready
      if (support) return null
      const link = await keys.pending(currentUser()).catch(() => null)
      return link ? { computerId: link.computerId, inviteId: link.inviteId, token: link.token } : null
    },
    keep: async (slot, inviteId, token) => {
      await keys.keepInvite(slot, { inviteId, token })
      // Kept per account now; the waiting copy has done its job.
      await keys.clearPending()
    },
    has: (slot) => keys.hasInvite(slot),
    kept: (slot) => keys.keptInvite(slot),
    forget: (slot) => keys.forgetInvite(slot),
  },
  updates: { state: async () => updates, onChange: none, check() {}, install() {}, setAutoCheck() {}, setAutoDownload() {} },
  files: { begin: unavailable, write: async () => {}, finish: async () => {}, cancel: async () => {} },
  app: {
    // Nothing leaves the page: an error report could carry what an agent wrote.
    reportError() {},
    // The same check the desktop main makes. A new tab with no opener; never a blob: or data: document (design §4.4).
    openExternal: (url) => {
      if (isSafeExternalURL(url)) window.open(url, '_blank', 'noopener,noreferrer')
    },
    openMemoryFile: () => {},
    copy: (text) => void navigator.clipboard?.writeText(text).catch(() => {}),
    pickFiles,
    pickFolder: async () => null,
    readClipboardFiles: async () => [],
    setTraySummary() {},
    raiseWindow() {},
    onCommand: none,
    resetAllData: async () => {},
    setLaunchAtLogin: async () => false,
    showInDock: async () => true,
    setShowInDock: async () => true,
    setAppearance: () => {},
    launchAtLogin: async () => false,
    openSettings() {},
    openSettingsWindow() {},
    profileMenu: async () => null,
    authenticate: unavailable,
    screenAgentState: async () => ({ available: false, needsApproval: false, registered: false }),
    setScreenAgent: unavailable,
    syncScreenAgent: async () => ({ needsApproval: false }),
  },
}

window.kith = bridge
