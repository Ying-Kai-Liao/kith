import type { RoomPerson } from '@shared/host-contract.gen'
import { grantRoles } from '@shared/room-people'
import type { Person, Role } from '@shared/people'
import { isUnsupportedHost } from '@shared/kith-info'
import { Observable } from '../lib/observable'
import type { BotStore } from './bot-store'
import { peopleFor } from './people-store'

const message = (e: unknown) => (e instanceof Error ? e.message : typeof e === 'object' && e && 'message' in e ? String(e.message) : String(e))

/** One group room's people, as the owner sees them: every stay, and whether this host turned people in rooms on. */
export class RoomPeopleModel extends Observable {
  /** null until the host answered once; false while the setting is off or the host is from before rooms with people. */
  enabled: boolean | null = null
  stays: RoomPerson[] = []
  gen = 0
  error: string | null = null
  busy = false

  constructor(private store: BotStore, readonly room: string) {
    super()
  }

  async load() {
    const client = this.store.connection.kind === 'online' ? this.store.client : null
    if (!client || !this.store.isOwner) return
    try {
      const r = await client.roomPeople(this.room)
      this.enabled = r.enabled
      this.stays = r.people
      this.gen = r.gen
    } catch (e) {
      if (isUnsupportedHost(e)) this.enabled = false
      else this.error = message(e)
    }
    this.changed()
  }

  /** Runs a change once at a time (a second tap while one is on its way does nothing), then re-reads both lists. */
  private async run(work: () => Promise<void>): Promise<boolean> {
    if (this.busy) return false
    this.busy = true
    this.error = null
    this.changed()
    try {
      await work()
      return true
    } catch (e) {
      this.error = message(e)
      return false
    } finally {
      this.busy = false
      await Promise.all([this.load(), peopleFor(this.store).load()])
      this.changed()
    }
  }

  add(personId: string) {
    return this.run(async () => void (await (await this.store.ready()).addRoomPerson(this.room, personId)))
  }

  remove(personId: string) {
    return this.run(async () => void (await (await this.store.ready()).removeRoomPerson(this.room, personId)))
  }

  /**
   * The grant card's one step: save access (merged with what the person holds), then add. A failed add says both
   * outcomes; the grant is never undone silently.
   */
  grantAndAdd(person: Person, given: Record<string, Role>, givenNames: string[]) {
    return this.run(async () => {
      await this.save(person, given)
      try {
        await (await this.store.ready()).addRoomPerson(this.room, person.personId)
      } catch (e) {
        throw new Error(`${person.name} now has ${givenNames.join(' and ')}, but couldn't join: ${message(e)}`)
      }
    })
  }

  /** Gives agents without adding (the needs-you row's "Give coder"). */
  give(person: Person, given: Record<string, Role>) {
    return this.run(() => this.save(person, given))
  }

  private async save(person: Person, given: Record<string, Role>) {
    const people = peopleFor(this.store)
    // The people model runs one change at a time and keeps its own `busy`; it is idle here (this model holds the lock).
    if (!(await people.setAccess(person.personId, grantRoles(person, given)))) throw new Error(people.error ?? "Couldn't give access.")
  }
}

const models = new WeakMap<BotStore, Map<string, RoomPeopleModel>>()

export function roomPeopleFor(store: BotStore, room: string): RoomPeopleModel {
  let byRoom = models.get(store)
  if (!byRoom) models.set(store, (byRoom = new Map()))
  let model = byRoom.get(room)
  if (!model) byRoom.set(room, (model = new RoomPeopleModel(store, room)))
  return model
}
