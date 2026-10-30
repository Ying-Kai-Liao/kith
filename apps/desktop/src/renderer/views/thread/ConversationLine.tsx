import { useEffect, useRef, useState } from 'react'
import { clampTurns, DEFAULT_TURNS, liveLine, MAX_TURNS, MIN_TURNS, TURN_PICKS, type Conversation } from '@shared/conversation'
import { isGroup } from '@shared/models'
import { BotAvatar } from '../../components/Avatar'
import { BotGuestAvatar } from '../../components/kith'
import { Icon } from '../../components/Icon'
import { useEscape } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { roomBotOf } from './room-bots'
import './conversation.css'

/**
 * One glass line above the box while a room run is open in the lane being viewed: whose turn it is, the turns left
 * with a short bar, and Stop. It stands in for the working indicator and the composer's Stop, so neither doubles it.
 */
export function LiveLine({ conversation, botId, lane }: { conversation: Conversation; botId: string; lane: string | null }) {
  const store = useStore()
  // A bot's wait counts down by the minute.
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    if (conversation.waitUntil === null) return
    setNow(Date.now())
    const timer = setInterval(() => setNow(Date.now()), 15_000)
    return () => clearInterval(timer)
  }, [conversation.waitUntil])
  const line = liveLine(conversation, now)
  const speaker = conversation.current
  const agent = speaker && speaker.kind === 'agent' ? store.bots.get(speaker.id) : undefined
  const guest = speaker?.kind === 'bot'
  const title = speaker ? (line.wait ? `${line.who} · ${line.wait}` : `${line.who} · answering`) : 'Working it out'
  return (
    <div className="live-line kith-glass" role="status">
      <span className="live-line-face">
        {guest ? <BotGuestAvatar provider={roomBotOf(store, store.bots.get(botId), speaker.id).provider} size={26} /> : agent ? <BotAvatar bot={agent} size={26} animated={false} /> : null}
      </span>
      <span className="live-line-text">
        <Text size="subheadline" weight="semibold" tone="text" truncate>{title}</Text>
        <span className="live-line-turns">
          <span className="live-line-bar" aria-hidden><span style={{ width: `${Math.round(line.fraction * 100)}%` }} /></span>
          <Text size="footnote" tone="secondary" tabular>{line.left}</Text>
        </span>
      </span>
      <button className="press live-line-stop" onClick={() => store.stop(botId, lane)} aria-label="Stop the room's run">
        <Icon name="stop.fill" size={10} weight="bold" /> Stop
      </button>
    </div>
  )
}

/** Where a run ended, in the chat: a quiet line, and for spent turns one button to give them more. */
export function EndLine({ botId, text, more }: { botId: string; text: string; more: boolean }) {
  const store = useStore()
  return (
    <div className="end-line">
      <span className="end-line-rule" />
      <Text size="footnote" tone="secondary" className="end-line-text">{text}</Text>
      {more ? <button className="press end-line-more" onClick={() => store.keepGoing(botId, DEFAULT_TURNS)}>Give them {DEFAULT_TURNS} more</button> : null}
      <span className="end-line-rule" />
    </div>
  )
}

/** The quiet "↻ 10" before Send in a group; ink when this message carries another number. */
export function TurnsChip({ turns, open, toggle }: { turns: number; open: boolean; toggle: () => void }) {
  return (
    <button
      className={`press turns-chip ${turns !== DEFAULT_TURNS ? 'changed' : ''} ${open ? 'open' : ''}`}
      aria-label={`${turns} turns for this message`}
      aria-expanded={open}
      title="How many turns this message gives the room"
      onMouseDown={(e) => e.preventDefault()}
      onClick={toggle}
    >
      <Icon name="arrow.clockwise" size={10} weight="semibold" /> {turns}
    </button>
  )
}

/** "Let them work it out": a stepper and five picks, over the chip. Up and Down step it from the box; Return sends. */
export function TurnsPopover({ turns, setTurns, close }: { turns: number; setTurns: (n: number) => void; close: () => void }) {
  const box = useRef<HTMLDivElement>(null)
  useEscape(true, close)
  useEffect(() => {
    const away = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null
      if (box.current?.contains(target) || target?.closest('.turns-chip')) return
      close()
    }
    document.addEventListener('mousedown', away)
    return () => document.removeEventListener('mousedown', away)
  }, [close])
  return (
    <div ref={box} className="turns-popover" role="dialog" aria-label="Let them work it out" onKeyDown={(e) => {
      if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return
      e.preventDefault()
      setTurns(clampTurns(turns + (e.key === 'ArrowUp' ? 1 : -1)))
    }}>
      <Text as="div" size="subheadline" weight="semibold" tone="text">Let them work it out</Text>
      <div className="turns-stepper">
        <button className="press turns-step" aria-label="One fewer turn" disabled={turns <= MIN_TURNS} onMouseDown={(e) => e.preventDefault()} onClick={() => setTurns(clampTurns(turns - 1))}>−</button>
        <span className="turns-number"><span className="turns-value" aria-live="polite">{turns}</span> <span className="turns-unit">{turns === 1 ? 'turn' : 'turns'}</span></span>
        <button className="press turns-step" aria-label="One more turn" disabled={turns >= MAX_TURNS} onMouseDown={(e) => e.preventDefault()} onClick={() => setTurns(clampTurns(turns + 1))}>+</button>
      </div>
      <div className="turns-picks">
        {TURN_PICKS.map((n) => (
          <button key={n} className={`press turns-pick ${n === turns ? 'on' : ''}`} aria-pressed={n === turns} onMouseDown={(e) => e.preventDefault()} onClick={() => setTurns(n)}>{n}</button>
        ))}
      </div>
      <Text as="div" size="footnote" tone="secondary">Each reply or pass is one turn. You can stop them any time.</Text>
    </div>
  )
}

/** Whether the chip belongs in this chat: only a group has a room to give turns to. */
export const showsTurns = (bot: Parameters<typeof isGroup>[0] | null | undefined) => !!bot && isGroup(bot)
