import { useEffect } from 'react'
import { isGroup } from '@shared/models'
import { openStay } from '@shared/room-people'
import type { Bot } from '@shared/models'
import { dimmedAgents, needsKai, roomBlock, roomParts, wontAnswer, type RoomAgent } from '@shared/room-people'
import { useModel, useModels } from '../../lib/observable'
import type { BotStore } from '../../store/bot-store'
import { useStore } from '../../store/context'
import { peopleFor } from '../../store/people-store'
import { roomPeopleFor } from '../../store/room-people-store'
import { roomBotOf } from './room-bots'

/**
 * What the owner's room views read about one group: its agents, whether it can take people, the stays and the people
 * behind them. Refreshed when the room opens and after every change (`RoomPeopleModel.run`). Everything is empty for
 * a member, and `enabled` is false while the host's `rooms.people` setting is off.
 */
export function useRoomFacts(group: Bot) {
  const store = useStore()
  const model = roomPeopleFor(store, group.id)
  const people = peopleFor(store)
  useModel(model)
  useModel(people)
  const online = store.connection.kind === 'online'
  useEffect(() => {
    if (!online || !store.isOwner) return
    void model.load()
    void people.load()
  }, [model, people, online, store])
  const parts = roomParts(group.members, (id) => store.bots.get(id), (id) => roomBotOf(store, group, id).name)
  const needs = needsKai(model.stays, people.people, parts.agents)
  return {
    model,
    people,
    // Off hides the section, unless someone still has an open stay: the owner can still remove them.
    enabled: (model.enabled === true || model.stays.some((s) => (s.leftGen ?? null) === null)) && people.supported !== false,
    agents: parts.agents as RoomAgent[],
    outside: parts.outside,
    block: roomBlock(parts),
    needs,
    wont: wontAnswer(needs, parts.agents),
  }
}

/** The group rooms (that take people) where this person has an open stay: what Settings warns about before access is taken away. */
export function usePersonRooms(store: BotStore | null, personId: string): { room: string; name: string; agents: RoomAgent[] }[] {
  const groups = store && personId ? store.roster.filter(isGroup) : []
  const models = groups.map((g) => roomPeopleFor(store!, g.id))
  useModels(models)
  useEffect(() => {
    if (store && store.connection.kind === 'online' && store.isOwner) for (const m of models) void m.load()
  }, [store, store?.connection.kind, models.length])
  return groups.flatMap((g, i) => {
    if (!openStay(models[i]!.stays, personId)) return []
    return [{ room: g.id, name: g.name, agents: roomParts(g.members, (id) => store!.bots.get(id), () => 'Bot').agents }]
  })
}

/** The agent ids a room's @ picker dims ("won't answer here"): empty for a 1:1, a member and a room with nothing wrong. */
export function useDimmedAgents(botId: string): Set<string> {
  const store = useStore()
  const model = roomPeopleFor(store, botId)
  const people = peopleFor(store)
  useModel(model)
  useModel(people)
  const bot = store.bots.get(botId)
  if (!bot || !store.isOwner || !isGroup(bot) || model.enabled !== true) return new Set()
  const parts = roomParts(bot.members, (id) => store.bots.get(id), () => 'Bot')
  return dimmedAgents(wontAnswer(needsKai(model.stays, people.people, parts.agents), parts.agents))
}
