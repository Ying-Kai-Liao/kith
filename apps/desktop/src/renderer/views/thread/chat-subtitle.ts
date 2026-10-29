import { isGroup, type Bot, type ScreenState } from '../../../shared/models.ts'
import type { Connection } from '../../store/bot-mirror.ts'
import type { StatusKind } from '../../components/kith/logic.ts'

export type ChatSubtitle =
  | { kind: 'chip'; chip: StatusKind; detail?: string }
  | { kind: 'using' }
  | null

interface Facts {
  bot: Bot | null | undefined
  /** This window's own computer: the only one whose screen setup the Mac app can show. */
  isLoopback: boolean
  isOwner: boolean
  screen: ScreenState | null
}

/** One agent chat whose agent may use this computer, for its owner. A group, a bot guest or someone else's agent has no entry. */
export function showsComputerItem({ bot, isLoopback, isOwner }: Facts): boolean {
  return !!bot && !isGroup(bot) && !bot.botGuest && bot.computer && isLoopback && isOwner
}

/** True while the chat's agent is driving the screen. */
export function usingComputer(facts: Facts): boolean {
  return showsComputerItem(facts) && !!facts.bot && facts.screen?.agentBot === facts.bot.id
}

/**
 * What sits under the name: a status chip when the connection isn't Live (it wins over "using the computer": a Mac
 * that can't be reached can't be watched), else "using the computer", else nothing.
 */
export function chatSubtitle(facts: Facts, connection: Connection, mismatch: boolean, hostName: string): ChatSubtitle {
  switch (connection.kind) {
    case 'connecting':
      return { kind: 'chip', chip: 'catching-up' }
    case 'computerOffline':
    case 'offline':
    case 'unauthorized':
    case 'unpaired':
      return { kind: 'chip', chip: 'mac-unreachable', detail: hostName }
    case 'online':
      if (mismatch) return { kind: 'chip', chip: 'machine', detail: 'Needs update' }
  }
  return usingComputer(facts) ? { kind: 'using' } : null
}
