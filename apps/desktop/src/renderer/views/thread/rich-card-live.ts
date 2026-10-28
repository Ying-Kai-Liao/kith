import { useEffect } from 'react'
import type { Entry } from '@shared/models'
import type { RichCard } from '@shared/rich-card'
import { liveKithJob, liveRoutine, type ScheduleLive } from '@shared/schedule-card'
import { Observable, useModel } from '../../lib/observable'
import { useStore, useStoreRef } from '../../store/context'
import type { BotStore } from '../../store/bot-store'
import type { KithInfoStore } from '../../store/kith-info-store'
import { useKithInfo } from '../bots/kith/use-kith'
import type { RoutineListing } from '../routines/routine'

/** Cards on screen read their job again this often; the host caches kith's answer for 30 s. */
const POLL_MS = 30_000

/** One `routines` listing per bot and host, shared by every card of that bot's routines. */
class RoutinePoll extends Observable {
  listing: RoutineListing | null = null
  at = 0
  private inflight: Promise<void> | null = null

  constructor(private readonly host: BotStore, private readonly botId: string) { super() }

  load(): Promise<void> {
    if (Date.now() - this.at < POLL_MS - 5000) return Promise.resolve()
    return (this.inflight ??= (async () => {
      try {
        this.listing = await (await this.host.ready()).call<RoutineListing>('routines', { botId: this.botId })
        this.at = Date.now()
        this.changed()
      } catch {
        // The card keeps what it last read, or its snapshot.
      } finally {
        this.inflight = null
      }
    })())
  }
}

const polls = new WeakMap<BotStore, Map<string, RoutinePoll>>()
function pollFor(host: BotStore, botId: string): RoutinePoll {
  let byBot = polls.get(host)
  if (!byBot) polls.set(host, (byBot = new Map()))
  let poll = byBot.get(botId)
  if (!poll) byBot.set(botId, (poll = new RoutinePoll(host, botId)))
  return poll
}

const kithLoads = new WeakMap<KithInfoStore, number>()
function loadKith(model: KithInfoStore) {
  if (Date.now() - (kithLoads.get(model) ?? 0) < POLL_MS - 5000) return
  kithLoads.set(model, Date.now())
  void model.load()
}

/**
 * The job a Schedule card shows, read live for the owner: a Kith job from `kithInfo` schedules, a routine from the
 * `routines` listing. Undefined while nothing is read, or for anyone else (members get no job state), so the card shows
 * its snapshot; null when the job is gone.
 */
export function useLiveSchedule(card: RichCard, entry: Entry): ScheduleLive | null | undefined {
  const host = useStoreRef()
  const store = useStore()
  const ref = card.ref ?? {}
  const routine = typeof ref.routine === 'string' ? ref.routine : null
  const agent = typeof ref.agent === 'string' ? ref.agent : null
  const owner = store.isOwner
  const jobBot = owner && agent ? ([...store.bots.values()].find((b) => b.hqAgent === agent)?.id ?? entry.botId) : ''
  const kith = useKithInfo(routine ? '' : jobBot)
  const routineBot = owner && routine && typeof ref.bot === 'string' ? ref.bot : null
  const poll = useModel(routineBot ? pollFor(host, routineBot) : null)

  useEffect(() => {
    const read = () => {
      if (kith) loadKith(kith)
      if (poll) void poll.load()
    }
    read()
    const timer = setInterval(read, POLL_MS)
    return () => clearInterval(timer)
  }, [kith, poll])

  if (routine) return poll?.listing ? liveRoutine(poll.listing, routine) : undefined
  const id = typeof ref.id === 'string' ? ref.id : null
  return kith?.info && id ? liveKithJob(kith.info.schedules, id) : undefined
}
