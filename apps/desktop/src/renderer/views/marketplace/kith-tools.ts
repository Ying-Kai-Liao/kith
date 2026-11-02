import { Observable, useModel } from '../../lib/observable'
import type { BotStore } from '../../store/bot-store'
import { skillHolders, targetBots, type AvailableConnector, type KithTools as AgentTools } from '@shared/kith-tools'
import type { Bot } from '@shared/models'
import { errorText } from './market-models'

/** How many kith agents are asked for their skills at once (each is a `kith` process). */
const SKILL_READS = 3

/**
 * Which kith agents hold which connectors and skills, for the Marketplace. Connectors come from one call
 * (`kith connectors`, which names the agents of each); skills are read agent by agent in the background.
 * Every change goes through the host's kith* methods, which run the CLI; nothing here keeps its own copy of a grant.
 */
export class KithToolsModel extends Observable {
  available: AvailableConnector[] = []
  /** Skills by agent name, as each agent's read comes in. */
  private skills = new Map<string, AgentTools>()
  /** Why the connector list could not be read (kith missing or broken); shown instead of a blank list. */
  error: string | null = null
  /** `agent:kind:name` of the changes in flight, so a double click does nothing. */
  private pending = new Set<string>()
  private generation = 0

  constructor(readonly store: BotStore) {
    super()
  }

  get bots(): Bot[] {
    return targetBots(this.store.bots.values())
  }

  isPending(bot: Bot, kind: 'connector' | 'skill', name: string) {
    return this.pending.has(`${bot.id}:${kind}:${name}`)
  }

  /** Agent names that have the connector `id`. */
  connectorHolders(id: string): string[] {
    return this.available.find((c) => c.name === id)?.connected ?? []
  }

  skillHolders(): Map<string, string[]> {
    return skillHolders(this.skills)
  }

  /** Bots (not agent names) among `agents`, in roster order. */
  botsOf(agents: readonly string[]): Bot[] {
    return this.bots.filter((b) => agents.includes(b.hqAgent!))
  }

  async refreshConnectors() {
    try {
      this.available = await (await this.store.ready()).kithConnectorsAvailable()
      this.error = null
    } catch (e) {
      this.error = errorText(e)
    }
    this.changed()
  }

  /** Reads every kith agent's skills, a few at a time; a bot that fails is skipped (its detail page says why). */
  async refreshSkills() {
    const run = ++this.generation
    const queue = [...this.bots]
    const client = await this.store.ready()
    const worker = async () => {
      for (let bot = queue.shift(); bot; bot = queue.shift()) {
        try {
          const t = await client.kithTools(bot.id, 'skills')
          if (run !== this.generation) return
          if (!t.skillsError) this.skills.set(bot.hqAgent!, t)
        } catch {}
        this.changed()
      }
    }
    await Promise.all(Array.from({ length: SKILL_READS }, worker))
  }

  async refresh() {
    await Promise.all([this.refreshConnectors(), this.refreshSkills()])
  }

  private async change(bot: Bot, kind: 'connector' | 'skill', name: string, op: (c: Awaited<ReturnType<BotStore['ready']>>) => Promise<unknown>) {
    const key = `${bot.id}:${kind}:${name}`
    if (this.pending.has(key)) return
    this.pending.add(key)
    this.changed()
    try {
      await op(await this.store.ready())
    } finally {
      this.pending.delete(key)
      // Read back what the CLI now says, never what we asked for.
      if (kind === 'connector') await this.refreshConnectors()
      else await this.readSkills(bot)
      this.changed()
    }
  }

  private async readSkills(bot: Bot) {
    try {
      const t = await (await this.store.ready()).kithTools(bot.id, 'skills')
      if (!t.skillsError) this.skills.set(bot.hqAgent!, t)
    } catch {}
  }

  connect(bot: Bot, connector: string) {
    return this.change(bot, 'connector', connector, (c) => c.kithConnect(bot.id, connector))
  }

  disconnect(bot: Bot, connector: string) {
    return this.change(bot, 'connector', connector, (c) => c.kithDisconnect(bot.id, connector))
  }

  addSkill(bot: Bot, skill: string) {
    return this.change(bot, 'skill', skill, (c) => c.kithSkillAdd(bot.id, skill))
  }

  removeSkill(bot: Bot, skill: string) {
    return this.change(bot, 'skill', skill, (c) => c.kithSkillRemove(bot.id, skill))
  }
}

const byStore = new WeakMap<BotStore, KithToolsModel>()

export function kithToolsFor(store: BotStore) {
  let k = byStore.get(store)
  if (!k) byStore.set(store, (k = new KithToolsModel(store)))
  return k
}

export const useKithTools = (store: BotStore) => useModel(kithToolsFor(store))
