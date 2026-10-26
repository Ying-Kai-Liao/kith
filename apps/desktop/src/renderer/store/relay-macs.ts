import { macRole, scopedKey, type MacRole } from '@shared/mac-role'
import { askForAccess, accessText } from '@shared/relay-access'
import type { Computer } from '@shared/models'
import type { RelayComputer } from '@shared/relay'
import { HostError, RelayTransport, type HostClient } from '../client/host-client'
import { Observable, useModel } from '../lib/observable'
import { pref, type Pref } from '../lib/prefs'
import { account } from './account'
import type { AppModel } from './app-model'
import { BotStore } from './bot-store'

/** Where asking one Mac for this device's access stands. */
export type MacAccess =
  | { kind: 'asking' }
  /** An owner compares these six digits with the ones on the Mac; a member is approved by the Mac without them. */
  | { kind: 'code'; code: string; compare: boolean }
  | { kind: 'failed'; message: string }

/** The Mac was reached before this device showed its invite. */
const UNPROVEN = /hasn't shown its invite/i
const ALREADY_PROVEN = /already shown its invite/i
export const OPEN_INVITE_LINK = 'Open the invite link on this computer.'

/** The Macs chosen on this device by this account: the ones to keep connected. Another account has its own list. */
const chosenLists = new Map<string, Pref<string[]>>()
const chosenOf = (user: string | null) => {
  const key = `relayMacs.${user ?? ''}`
  if (!chosenLists.has(key)) chosenLists.set(key, pref<string[]>(key, []))
  return chosenLists.get(key)!
}

/**
 * Other Macs over the Kith cloud: the account's list (owned and member), and the ones connected on this device, each a
 * normal computer of the app model (`HostClient` over the relay). Loopback and SSH computers are not touched here.
 */
export class RelayMacs extends Observable {
  computers: RelayComputer[] = []
  error: string | null = null
  loaded = false
  access = new Map<string, MacAccess>()
  private attached = new Map<string, string>()
  private tokens = new Map<string, boolean>()
  private asking = new Map<string, { cancelled: boolean }>()
  private user: string | null = null

  private get chosen() {
    return chosenOf(this.user)
  }

  constructor(private app: AppModel) {
    super()
    account.subscribe(() => this.noteAccount())
    this.noteAccount()
  }

  private noteAccount() {
    const user = account.user?.userId ?? null
    if (user === this.user) return
    this.user = user
    // Another account (or none) starts clean: nothing of the earlier one stays attached.
    for (const id of [...this.attached.keys()]) this.unattach(id)
    for (const ask of this.asking.values()) ask.cancelled = true
    this.asking.clear()
    this.access.clear()
    this.tokens.clear()
    this.computers = []
    this.loaded = false
    this.changed()
    if (user) void this.refresh()
  }

  isChosen(id: string) {
    return this.chosen.get().includes(id)
  }

  /** The row for a Mac, from the account's list. */
  row(id: string) {
    return this.computers.find((c) => c.computerId === id) ?? null
  }

  store(id: string) {
    return this.attached.has(id) ? this.app.store(id) : null
  }

  /** The role on a Mac that is on this list: the connected store knows best, the cloud's row before that. */
  roleOf(id: string): MacRole {
    const store = this.store(id)
    if (store) return store.role
    return macRole({ granted: null, cloudRole: this.row(id)?.role ?? null, hasToken: this.tokens.get(id) ?? false })
  }

  async refresh() {
    if (!account.isSignedIn) return
    const res = await window.kith.relay.computers()
    if (res.ok) {
      this.computers = res.value
      this.error = null
      // Only whether a token is kept; the token itself stays in the main process.
      for (const c of res.value) if (!this.tokens.has(c.computerId)) this.tokens.set(c.computerId, await window.kith.invites.has(this.keyOf(c.computerId)))
    } else {
      this.error = res.error.message ?? "Can't reach the Kith cloud."
    }
    this.loaded = true
    this.sync()
    this.changed()
  }

  /** Attaches the chosen Macs this device may connect to, and detaches the rest. */
  private sync() {
    const wanted = new Map<string, RelayComputer>()
    for (const id of this.chosen.get()) {
      const c = this.row(id)
      // Not on the list any more (removed from the account): nothing to connect to, nothing cached to keep.
      if (c && c.access === 'granted') wanted.set(id, c)
    }
    for (const id of [...this.attached.keys()]) if (!wanted.has(id)) this.unattach(id)
    for (const [id, c] of wanted) if (this.attached.get(id) !== c.signKey) this.attach(c)
  }

  private attach(c: RelayComputer) {
    const computer: Computer = { id: c.computerId, name: c.name, signKey: c.signKey, urls: [] }
    // The cache is this account's: contextId keeps another account on this device from seeing it.
    const store = new BotStore(computer, false, () => new RelayTransport(c.computerId, c.signKey), undefined, `account:${this.user ?? ''}`)
    store.fixedRoute = 'relay'
    store.roleHint = { cloudRole: c.role, hasToken: this.tokens.get(c.computerId) ?? false }
    store.beforeConnect = (client) => this.showInvite(c.computerId, client)
    store.explain = (error) => this.explainRefusal(error)
    this.attached.set(c.computerId, c.signKey)
    this.app.attach(computer, store)
    // Access taken away on the Mac or the cloud (4003): say so, and drop what was cached from it.
    store.subscribe(() => {
      // Whether or not the Mac ever answered on this launch: a cache from an earlier one is dropped too.
      if (store.connection.kind === 'unauthorized' && !store.dropped) store.dropData()
    })
  }

  private unattach(id: string) {
    this.attached.delete(id)
    this.app.detach(id)
    void window.kith.relay.forget(id)
  }

  /** A device of an invited person shows its invite before anything else, on every connection. */
  private async showInvite(computerId: string, client: HostClient) {
    const kept = await window.kith.invites.kept(this.keyOf(computerId))
    if (!kept) return
    this.tokens.set(computerId, true)
    try {
      await client.proveInvite(kept.inviteId, kept.token)
    } catch (e) {
      if (ALREADY_PROVEN.test(e instanceof Error ? e.message : '')) return
      // The Mac said no (not a lost link): retrying would send the token again for the same answer.
      if (e instanceof HostError && e.status !== 0) throw new HostError(e.status, e.message, 'refused')
      throw e
    }
  }

  /** What the Mac answers on purpose is shown in its words and not retried; a lost link or a lease is retried as usual. */
  private explainRefusal(error: unknown) {
    if (!(error instanceof Error)) return null
    if (UNPROVEN.test(error.message)) return { message: OPEN_INVITE_LINK, final: true }
    if (error instanceof HostError && error.code === 'refused') return { message: error.message, final: true }
    return null
  }

  private keyOf(computerId: string) {
    return scopedKey(this.user ?? '', computerId)
  }

  /** Connects a Mac: asks for access first when this device has none yet. Repeats while one is going are one request. */
  async connect(id: string) {
    const c = this.row(id)
    if (!c || this.asking.has(id)) return
    if (!this.chosen.get().includes(id)) this.chosen.set([...this.chosen.get(), id])
    if (c.access === 'granted') {
      // A store that gave up on "access removed" is replaced by a fresh one.
      if (this.store(id)?.connection.kind === 'unauthorized') this.unattach(id)
      void window.kith.relay.restart(id)
      this.sync()
      this.store(id)?.restartStream()
      return this.changed()
    }
    const mine = { cancelled: false }
    this.asking.set(id, mine)
    this.access.set(id, { kind: 'asking' })
    this.changed()
    try {
      const outcome = await askForAccess(window.kith.relay, c, {
        cancelled: () => mine.cancelled,
        onCode: (t) => {
          this.access.set(id, { kind: 'code', code: t.code, compare: c.role === 'owner' })
          this.changed()
        },
      })
      if (outcome === 'approved') {
        this.access.delete(id)
        await this.refresh()
        void window.kith.relay.restart(id)
      } else if (outcome === 'cancelled') this.access.delete(id)
      else this.access.set(id, { kind: 'failed', message: accessText(outcome) })
    } catch (e) {
      this.access.set(id, { kind: 'failed', message: e instanceof Error ? e.message : "Couldn't ask the Mac for access." })
    } finally {
      this.asking.delete(id)
      this.changed()
    }
  }

  /**
   * Tries a Mac again: the main process restarts its relay link (the same call `connect` makes for an approved Mac)
   * and the store opens a fresh event stream over it. Access is left alone, so it never asks the Mac again.
   */
  reconnect(id: string) {
    void window.kith.relay.restart(id)
    this.store(id)?.restartStream()
  }

  cancel(id: string) {
    const mine = this.asking.get(id)
    if (mine) mine.cancelled = true
  }

  /** Stops using a Mac on this device: its channel closes, its cached chats go, and an invite kept for it is forgotten. */
  async disconnect(id: string) {
    this.cancel(id)
    this.chosen.set(this.chosen.get().filter((x) => x !== id))
    this.store(id)?.dropData()
    this.unattach(id)
    this.access.delete(id)
    if (this.tokens.get(id)) {
      this.tokens.set(id, false)
      try {
        await window.kith.invites.forget(this.keyOf(id))
      } catch {}
    }
    this.changed()
  }

  /** The invitee's join finished: this Mac is one to keep connected. */
  async adopt(id: string) {
    if (!this.chosen.get().includes(id)) this.chosen.set([...this.chosen.get(), id])
    this.tokens.set(id, true)
    await this.refresh()
    this.store(id)?.restartStream()
  }
}

export function useRelayMacs(app: AppModel) {
  return useModel(app.relayMacs)
}
