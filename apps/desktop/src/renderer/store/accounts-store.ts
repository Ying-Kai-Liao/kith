import { emptyAccounts, type AccountAdd, type Accounts, type Policy } from '@shared/accounts'
import { isUnsupportedHost } from '@shared/kith-info'
import type { HostClient } from '../client/host-client'
import { Observable } from '../lib/observable'

const message = (error: unknown) => (error instanceof Error ? error.message : String(error))

/**
 * One computer's managed account set (`accounts` and the writes beside it). A view makes it when Settings → Accounts
 * opens; nothing polls. Every write answers with the fresh read, which replaces `accounts` (no second fetch); writes
 * go one after the other, so the last one sent is the one that stays. While any write is in flight `saving` is true
 * and the buttons stay off, so a double click can't send twice.
 */
export class AccountsStore extends Observable {
  accounts: Accounts = emptyAccounts
  /** Why the read failed, or why the last write did. */
  error: string | null = null
  loading = false
  loaded = false
  /** A host from before the account set: the page says to update. */
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
      const got = await (await this.client()).accounts()
      if (request === this.request) {
        this.accounts = got
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

  /** Sends one write; its answer is the new read. A failure shows its words and keeps what was shown. */
  private write(send: (client: HostClient) => Promise<Accounts>): Promise<boolean> {
    this.error = null
    this.saves++
    this.changed()
    const run = async (): Promise<boolean> => {
      try {
        const got = await send(await this.client())
        // A read that started before this write would land after it with older state: this answer wins.
        this.request++
        this.accounts = got
        this.loading = false
        this.loaded = true
        this.unsupported = false
        return true
      } catch (error) {
        this.error = message(error)
        return false
      } finally {
        this.saves--
        this.changed()
      }
    }
    const result = this.queue.then(run)
    this.queue = result.then(() => {})
    return result
  }

  /** `agent` null is the global set; `ids` null drops the agent's own list. */
  setList(agent: string | null, ids: string[] | null) { return this.write((c) => c.accountsSet(agent, ids)) }
  setPolicy(policy: Policy) { return this.write((c) => c.accountPolicy(policy)) }
  pin(agent: string, id: string, only: boolean) { return this.write((c) => c.accountPin(agent, id, only)) }
  unpin(agent: string) { return this.write((c) => c.accountUnpin(agent)) }
  setCrossHarness(agent: string | null, value: 'on' | 'off' | 'reset') { return this.write((c) => c.accountCrossHarness(agent, value)) }
  clear(id: string) { return this.write((c) => c.accountClear(id)) }
  remove(id: string) { return this.write((c) => c.accountRemove(id)) }

  /** Adds a login: the host makes the folder and starts the login terminal (the view shows `term`). Throws the host's words. */
  async add(harness: string, name: string, yes: boolean, cols: number, rows: number): Promise<AccountAdd> {
    this.error = null
    this.saves++
    this.changed()
    try {
      return await (await this.client()).accountAdd(harness, name, yes, cols, rows)
    } catch (error) {
      this.error = message(error)
      throw error
    } finally {
      this.saves--
      this.changed()
    }
  }
}
