import { emptyBackups, type Backups } from '@shared/backups'
import { isUnsupportedHost } from '@shared/kith-info'
import type { HostClient } from '../client/host-client'
import { Observable } from '../lib/observable'

const message = (error: unknown) => (error instanceof Error ? error.message : String(error))

/**
 * One computer's backup chain and main accounts (`backups`, `backupsSet`, `accountSet`). A view makes it when Settings or a bot's details
 * open; nothing polls. A reorder shows at once and goes to the host after the one before it, so two quick
 * reorders end on the last; what the host answers to the last one is what stays.
 */
export class BackupsStore extends Observable {
  backups: Backups = emptyBackups
  /** Why kith had no answer, or why the last save failed. */
  error: string | null = null
  loading = false
  loaded = false
  /** A host that doesn't know the call: the section stays hidden. */
  unsupported = false
  private request = 0
  private saves = 0
  private queue: Promise<void> = Promise.resolve()

  constructor(private readonly client: () => Promise<HostClient>) { super() }

  get saving() { return this.saves > 0 }

  async load(): Promise<void> {
    const request = ++this.request
    this.loading = true
    this.changed()
    try {
      const got = await (await this.client()).backups()
      if (request === this.request) {
        this.backups = got
        this.error = null
        this.unsupported = false
      }
    } catch (error) {
      if (request === this.request) {
        if (isUnsupportedHost(error)) this.unsupported = true
        else this.error = message(error)
      }
    } finally {
      if (request === this.request) {
        this.loading = false
        this.loaded = true
        this.changed()
      }
    }
  }

  /** `agent` null is the global order; `chain` null makes the agent follow the global order again. */
  set(agent: string | null, chain: string[] | null): Promise<void> {
    const request = ++this.request
    const next = { ...this.backups, agents: { ...this.backups.agents } }
    if (agent === null) next.global = chain ?? []
    else if (chain === null) delete next.agents[agent]
    else next.agents[agent] = chain
    this.backups = next
    this.error = null
    this.saves++
    this.changed()
    const send = async () => {
      try {
        const got = await (await this.client()).backupsSet(agent, chain)
        if (request === this.request) this.backups = got
      } catch (error) {
        this.error = message(error)
        // The host keeps what it had: show that, not the order that failed to save.
        if (request === this.request) await this.reread()
      } finally {
        this.saves--
        this.changed()
      }
    }
    return (this.queue = this.queue.then(send))
  }

  /** The main account of `agent` (null: the computer's); `account` null goes back to the default. Shows on the host's answer. */
  setMain(agent: string | null, account: string | null): Promise<void> {
    const request = ++this.request
    this.error = null
    this.saves++
    this.changed()
    const send = async () => {
      try {
        const got = await (await this.client()).accountSet(agent, account)
        if (request === this.request) this.backups = got
      } catch (error) {
        this.error = message(error)
        if (request === this.request) await this.reread()
      } finally {
        this.saves--
        this.changed()
      }
    }
    return (this.queue = this.queue.then(send))
  }

  private async reread() {
    try {
      this.backups = await (await this.client()).backups()
    } catch {
      // The error from the save stays on screen.
    }
  }
}
