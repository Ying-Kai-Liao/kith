import { memo } from 'react'
import { avatarImage, DEFAULT_IMAGE } from '@shared/crew'
import { botGuestTag } from '@shared/bot-guest-chat'
import { folderName, isGroup, isWorking, needsInput, type Bot } from '@shared/models'
import { Icon } from '../components/Icon'
import { BotAvatar, CountBadge, OriginTag, SkyPatch } from '../components/kith'
import type { Face } from '../components/Avatar'
import { LiveBot } from '../components/Avatar'
import { messagePreview } from '../lib/message-preview'
import type { BotStore } from '../store/bot-store'
import { rowTime } from './crew/row-time'
import './crew/crew-sidebar.css'

const faceOf = (bot: Bot): Face => (isGroup(bot) ? 'group' : (avatarImage(bot) ?? DEFAULT_IMAGE))

/**
 * A Crew row (the mock's Mac density): the bot at 40 with no tile, its name, the time or state at the right, one
 * status line (ink when it needs you) and the ink count of questions waiting. In the rail only the bot at 36 shows.
 */
export const BotRow = memo(function BotRow({ bot, store, compact, usingComputer, members, waiting }: {
  bot: Bot
  store: BotStore
  compact: boolean
  usingComputer: boolean
  members: Bot[]
  /** Questions waiting on you in this chat. */
  waiting: number
}) {
  const working = isWorking(bot) && !needsInput(bot)
  const state = working ? 'working' : 'idle'
  const count = needsInput(bot) ? Math.max(1, waiting) : waiting
  if (compact) {
    return (
      <span className="cs-rail-face">
        <RowFace bot={bot} members={members} size={36} state={state} />
      </span>
    )
  }
  const group = isGroup(bot)
  return (
    <span className="cs-row">
      <RowFace bot={bot} members={members} size={40} state={state} />
      <span className="cs-row-main">
        <span className="cs-row-top">
          {bot.pinned ? <Icon name="pin.fill" size={9} color="var(--kith-muted)" /> : null}
          <span className={`cs-row-name ${group ? 'group' : ''}`}>{bot.name}</span>
          {usingComputer ? <Icon name="cursorarrow.motionlines" size={10} color="var(--accent)" title="Using the computer" /> : null}
          {count > 0 ? null : <span className="cs-row-right">{bot.unread > 0 ? <i className="cs-unread" aria-label="Unread" /> : null}{rightText(bot, store)}</span>}
          {count > 0 ? <span className="cs-row-right"><CountBadge count={count} /></span> : null}
        </span>
        {bot.botGuest ? <span className="cs-row-status"><OriginTag origin="bot" label={botGuestTag(bot.botGuest)} /></span> : null}
        <span className={`cs-row-status ${count > 0 || bot.status === 'error' ? 'ink' : ''}`}>{statusLine(bot, store)}</span>
      </span>
    </span>
  )
})

function RowFace({ bot, members, size, state }: { bot: Bot; members: Bot[]; size: 36 | 40; state: 'working' | 'idle' }) {
  if (isGroup(bot) && members.length >= 2) {
    const [a, b] = members
    return (
      <span className="cs-duo" style={{ width: size, height: size }} aria-hidden>
        <span className="cs-duo-a kc-bot-shadow"><LiveBot face={faceOf(a!)} color={a!.avatarColor} size={26} id={a!.id} animated={false} /></span>
        <span className="cs-duo-b kc-bot-shadow"><LiveBot face={faceOf(b!)} color={b!.avatarColor} size={26} id={b!.id} animated={false} /></span>
        <span className="cs-duo-sky"><SkyPatch id={bot.id} size={15} /></span>
      </span>
    )
  }
  return <BotAvatar face={faceOf(bot)} color={bot.avatarColor} id={bot.id} size={size} state={state} />
}

/** At the row's right: "working" while it is, the offline state of its computer, else the time of its last message. */
function rightText(bot: Bot, store: BotStore): string {
  if (isWorking(bot)) return 'working'
  if (store.isOffline) return 'offline'
  return rowTime(bot.lastAt)
}

/** The status line: what it needs, what it is doing, or its last message. */
function statusLine(bot: Bot, store: BotStore): string {
  if (needsInput(bot)) return bot.activity || 'Needs your approval'
  if (isWorking(bot)) return bot.activity || 'Working…'
  if (bot.status === 'error') return messagePreview(bot.lastMessage ?? 'Something went wrong')
  return messagePreview(bot.lastMessage ?? (isGroup(bot) ? `${bot.members.length} bots` : `${store.backendName(bot.backend)} · ${folderName(bot)}`))
}
