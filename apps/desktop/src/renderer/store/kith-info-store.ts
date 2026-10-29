import { isUnsupportedHost, type KithInfo, type KithMemoryHit } from '@shared/kith-info'
import type { HostClient } from '../client/host-client'
import { Observable } from '../lib/observable'

const message = (error: unknown) => (error instanceof Error ? error.message : String(error))

/**
 * One Kith bot's details (`kithInfo`). A view makes one when it opens, so the page is read again on
 * reopen; bot events don't touch it and nothing polls. `unsupported` is a host that doesn't know the
 * call: the Kith sections stay hidden then.
 */
export class KithInfoStore extends Observable {
  info: KithInfo | null = null
  /** Why kith had no answer (the host's `error`), or why the first call failed. */
  error: string | null = null
  /** Why a refresh failed while `info` is on screen; the old details stay. */
  refreshError: string | null = null
  fetchedAt: number | null = null
  loading = false
  /** A call has finished (with or without details). */
  loaded = false
  unsupported = false
  private request = 0
  private inflight: Promise<void> | null = null
  private inflightForced = false
  private queued: Promise<void> | null = null

  constructor(readonly botId: string, private readonly client: () => Promise<HostClient>) { super() }

  /** A second call while one runs joins it, so a double click makes one request; a forced one that
   *  arrives during a plain load runs right after it, so the refresh is never swallowed. */
  load(refresh = false): Promise<void> {
    if (!this.inflight) return this.run(refresh)
    if (!refresh || this.inflightForced) return this.inflight
    return (this.queued ??= this.inflight.then(() => {
      this.queued = null
      return this.run(true)
    }))
  }

  /** Why the last permission change failed (hq refused it, or the host was unreachable); the old value stays shown. */
  permissionError: string | null = null
  permissionSaving = false

  /** Writes the mode through to hq. What shows afterwards is hq's own answer, read back, never the pick itself. */
  async setPermission(mode: string): Promise<void> {
    this.permissionSaving = true
    this.permissionError = null
    this.changed()
    try {
      const result = await (await this.client()).kithSetPermission(this.botId, mode)
      if (result.info) {
        this.info = result.info
        this.error = null
        this.refreshError = null
        this.fetchedAt = result.fetchedAt
      } else this.permissionError = result.error ?? 'Kith gave no answer'
    } catch (error) {
      this.permissionError = message(error)
    }
    this.permissionSaving = false
    this.changed()
  }

  /** Why the last call-model change failed; the old value stays shown. */
  callModelError: string | null = null
  callModelSaving = false

  /** Writes the call model through to hq; what shows afterwards is hq's answer, read back. */
  async setCallModel(model: string): Promise<void> {
    this.callModelSaving = true
    this.callModelError = null
    this.changed()
    try {
      const result = await (await this.client()).kithSetCallModel(this.botId, model)
      if (result.info) {
        this.info = result.info
        this.error = null
        this.refreshError = null
        this.fetchedAt = result.fetchedAt
      } else this.callModelError = result.error ?? 'Kith gave no answer'
    } catch (error) {
      this.callModelError = message(error)
    }
    this.callModelSaving = false
    this.changed()
  }

  /** A Schedule card's button: kith-sched does it, and the info read back is what every card of this agent shows. Throws why it failed. */
  async schedule(id: string, op: 'pause' | 'resume' | 'run' | 'remove'): Promise<void> {
    const result = await (await this.client()).kithSchedule(this.botId, id, op)
    if (!result.info) throw new Error(result.error ?? 'Kith gave no answer')
    this.info = result.info
    this.error = null
    this.refreshError = null
    this.fetchedAt = result.fetchedAt
    this.changed()
  }

  private run(refresh: boolean): Promise<void> {
    const request = ++this.request
    this.loading = true
    this.inflightForced = refresh
    this.changed()
    this.inflight = (async () => {
      try {
        const result = await (await this.client()).kithInfo(this.botId, refresh)
        if (request !== this.request) return
        this.unsupported = false
        if (!result.info && result.error && this.info) {
          // The host answers 200 when kith fails: the details on screen stay, with the reason beside refresh.
          this.refreshError = result.error
        } else {
          this.info = result.info
          this.error = result.info ? null : result.error
          this.refreshError = null
          this.fetchedAt = result.fetchedAt
        }
      } catch (error) {
        if (request !== this.request) return
        if (isUnsupportedHost(error)) this.unsupported = true
        // A failed refresh must not blank the page: it says so beside the refresh button.
        else if (this.info) this.refreshError = message(error)
        else this.error = message(error)
      } finally {
        if (request === this.request) {
          this.loading = false
          this.loaded = true
          this.inflight = null
          this.changed()
        }
      }
    })()
    return this.inflight
  }
}

/** Searches an agent's memory as the user types: debounced, and an answer to an older query is dropped. */
export class KithMemorySearch extends Observable {
  query = ''
  hits: KithMemoryHit[] = []
  error: string | null = null
  searching = false
  private request = 0
  private timer: ReturnType<typeof setTimeout> | null = null

  constructor(readonly botId: string, private readonly client: () => Promise<HostClient>, private readonly debounceMs = 300) { super() }

  setQuery(query: string) {
    this.query = query
    this.request++
    if (this.timer) clearTimeout(this.timer)
    this.timer = null
    if (!query.trim()) {
      this.hits = []
      this.error = null
      this.searching = false
      this.changed()
      return
    }
    this.searching = true
    this.changed()
    const request = this.request
    this.timer = setTimeout(() => void this.run(query.trim(), request), this.debounceMs)
  }

  dispose() {
    this.request++
    if (this.timer) clearTimeout(this.timer)
    this.timer = null
  }

  private async run(query: string, request: number) {
    try {
      const result = await (await this.client()).kithMemorySearch(this.botId, query, 30)
      if (request !== this.request) return
      this.hits = result.hits
      this.error = result.error
    } catch (error) {
      if (request !== this.request) return
      this.hits = []
      this.error = message(error)
    }
    this.searching = false
    this.changed()
  }
}
