import { isUnsupportedHost } from '@shared/kith-info'
import { agentsWithout } from '@shared/agent-share'
import type { AuditRow, CreatedInvite, PendingInvite, Person, Role } from '@shared/people'
import type { HostClient } from '../client/host-client'
import { Observable } from '../lib/observable'
import type { BotStore } from './bot-store'

const message = (e: unknown) => (e instanceof Error ? e.message : typeof e === 'object' && e && 'message' in e ? String(e.message) : String(e))

/** The people an owner invited on one computer, their pending invites and what they did there. */
export class PeopleModel extends Observable {
  /** null until the host answered once; false for a host before invites (the People page hides). */
  supported: boolean | null = null
  people: Person[] = []
  invites: PendingInvite[] = []
  audit: AuditRow[] = []
  error: string | null = null
  /** A host warning from the last change that still went through. */
  notice: string | null = null
  busy = false
  /** Who the owner just removed, so the page can offer Undo until they do something else. */
  removed: { personId: string; name: string } | null = null

  constructor(private store: BotStore) {
    super()
  }

  private async run<T>(work: (client: HostClient) => Promise<T>): Promise<T | null> {
    if (this.busy) return null
    this.busy = true
    this.error = null
    this.changed()
    try {
      return await work(await this.store.ready())
    } catch (e) {
      this.error = message(e)
      return null
    } finally {
      this.busy = false
      this.changed()
    }
  }

  /** Reads the lists; the first answer says whether this host knows people at all. */
  async load() {
    const client = this.store.connection.kind === 'online' ? this.store.client : null
    if (!client) return
    try {
      const [people, invites] = await Promise.all([client.people(), client.invites()])
      this.people = people
      this.invites = invites
      this.supported = true
      this.error = null
    } catch (e) {
      if (isUnsupportedHost(e)) this.supported = false
      else this.error = message(e)
    }
    this.changed()
  }

  /** `who` is a person's id or a new name; null when it failed (the reason is in `error`). */
  async create(who: { personId: string } | { name: string }, agents: Record<string, Role>): Promise<CreatedInvite | null> {
    const made = await this.run((c) => c.createInvite(who, agents))
    if (made) await this.load()
    return made
  }

  async revokeInvite(inviteId: string) {
    if ((await this.run((c) => c.revokeInvite(inviteId))) !== null) await this.load()
  }

  async setAccess(personId: string, agents: Record<string, Role>, projects?: string[]) {
    const done = await this.run((c) => c.setPersonAccess(personId, agents, projects))
    if (done !== null) await this.load()
    return done !== null
  }

  /** Takes one agent away from a person; they stay, with whatever else they hold. Host warnings land in `notice`. */
  async removeAgent(person: Person, agent: string) {
    const done = await this.run((c) => c.setPersonAccess(person.personId, agentsWithout(person, agent)))
    if (done === null) return
    const warnings = (done as { warnings?: unknown } | undefined)?.warnings
    this.notice = Array.isArray(warnings) && warnings.length ? warnings.map(String).join(' ') : null
    await this.load()
  }

  async removePerson(personId: string) {
    const name = this.people.find((p) => p.personId === personId)?.name ?? personId
    if ((await this.run((c) => c.revokePerson(personId))) === null) return
    this.removed = { personId, name }
    await this.load()
  }

  /** Undo of a revoke, from the banner or from the revoked person's row. The host's reason lands in `error`. */
  async restorePerson(personId: string) {
    if ((await this.run((c) => c.unrevokePerson(personId))) === null) return
    if (this.removed?.personId === personId) this.removed = null
    await this.load()
  }

  dismissRemoved() {
    this.removed = null
    this.changed()
  }

  async loadAudit(personId: string | null = null) {
    const rows = await this.run((c) => c.audit(0, personId))
    if (rows) {
      this.audit = rows
      this.changed()
    }
  }
}

const models = new WeakMap<BotStore, PeopleModel>()

export function peopleFor(store: BotStore): PeopleModel {
  let model = models.get(store)
  if (!model) models.set(store, (model = new PeopleModel(store)))
  return model
}
