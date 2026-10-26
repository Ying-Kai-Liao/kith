import type { ReactNode } from 'react'
import { avatarImage, DEFAULT_IMAGE } from '@shared/crew'
import { needsYouRows, projectDetail, type BoardDecision, type BoardModel, type BoardTarget } from '@shared/board'
import { isGroup, type Bot } from '@shared/models'
import { Icon } from '../../components/Icon'
import { BotAvatar, CountBadge } from '../../components/kith'
import { sameRef, type BotRef } from '../../store/app-model'
import type { BotStore } from '../../store/bot-store'
import { useModel } from '../../lib/observable'
import { GuestPromptList } from '../GuestPromptList'
import { pairOf } from '../../components/kith/pair.ts'
import { LimitCard } from '../thread/LimitCard'

/**
 * The Board in the sidebar: Needs you (a row per thread, with its count), Working, Where we left off and Projects.
 * A row opens the room its thread is in. Only the layout lives here; the sections are `buildBoard`'s.
 */
export function BoardList({ board, selected, bots, stores, open }: {
  board: BoardModel
  selected: BotRef | null
  /** A room's bot, to draw its face; absent for a room this window doesn't know. */
  bots: (t: BoardTarget) => Bot | undefined
  /** The shown computers whose guests' requests are answered in place. */
  stores: BotStore[]
  open: (t: BoardTarget) => void
}) {
  const needs = needsYouRows(board.needsYou)
  const isSelected = (t: BoardTarget) => sameRef(t, selected)
  const empty = !board.needsYou.length && !board.working.length && !board.leftOff.length && !board.projects.length
  return (
    <div className="cs-board">
      <Section title="Needs you" count={board.needsYou.length} show={board.needsYou.length > 0}>
        {stores.map((s) => <GuestPrompts key={s.computer.id} store={s} />)}
        {board.needsYou.map((d) => {
          const store = d.limit && stores.find((s) => s.computer.id === d.target.computerId)
          return d.limit && store ? <LimitPrompt key={d.id} store={store} decision={d} bot={bots(d.target)} /> : null
        })}
        {needs.map((n) => (
          <BoardRow key={n.key} bot={bots(n.target)} title={n.title} detail={`your turn · ${n.question}`} ink count={n.count} selected={isSelected(n.target)} onClick={() => open(n.target)} />
        ))}
      </Section>
      <Section title="Working" count={board.working.length} show={board.working.length > 0}>
        {board.working.map((w) => (
          <BoardRow key={w.id} bot={w.bot} title={w.title} detail={w.detail} selected={isSelected(w.target)} onClick={() => open(w.target)} />
        ))}
      </Section>
      <Section title="Where we left off" count={board.leftOff.length} show={board.leftOff.length > 0}>
        {board.leftOff.map((h) => (
          <BoardRow key={h.id} bot={h.bot} title={h.botName} detail={h.note} selected={isSelected(h.target)} onClick={() => open(h.target)} />
        ))}
      </Section>
      <Section title="Projects" count={board.projects.length} show={board.projects.length > 0}>
        {board.projects.map((p) => (
          <BoardRow key={p.id} icon="briefcase" title={p.name} detail={projectDetail(p)} selected={isSelected(p.target)} onClick={() => open(p.target)} />
        ))}
      </Section>
      {empty ? <div className="cs-board-empty">Nothing is waiting on you.</div> : null}
    </div>
  )
}

function GuestPrompts({ store }: { store: BotStore }) {
  useModel(store)
  return <GuestPromptList store={store} />
}

/** An agent at its limit with an owner's run waiting: answered here, in the agent's colours. */
function LimitPrompt({ store, decision, bot }: { store: BotStore; decision: BoardDecision; bot?: Bot }) {
  useModel(store)
  const entry = decision.limit!
  return (
    <LimitCard
      entry={entry}
      pair={pairOf(bot?.avatarColor)}
      answering={store.answering.get(entry.id) ?? null}
      respond={(option) => store.respond(entry, option)}
      leading={bot ? <BotAvatar face={avatarImage(bot) ?? DEFAULT_IMAGE} color={bot.avatarColor} id={bot.id} size={24} animated={false} /> : undefined}
      board
    />
  )
}

function Section({ title, count, show, children }: { title: string; count: number; show: boolean; children: ReactNode }) {
  if (!show) return null
  return (
    <section className="cs-section" aria-label={title}>
      <h3 className="cs-section-title">
        <span>{title}</span>
        <span>{count}</span>
      </h3>
      {children}
    </section>
  )
}

function BoardRow({ bot, icon, title, detail, ink = false, count = 0, selected, onClick }: {
  bot?: Bot
  icon?: string
  title: string
  detail: string
  ink?: boolean
  count?: number
  selected: boolean
  onClick: () => void
}) {
  return (
    <button className={`cs-card ${selected ? 'selected' : ''}`} aria-current={selected || undefined} onClick={onClick}>
      <span className="cs-card-face">
        {bot ? (
          <BotAvatar face={isGroup(bot) ? 'group' : (avatarImage(bot) ?? DEFAULT_IMAGE)} color={bot.avatarColor} id={bot.id} size={24} animated={false} />
        ) : (
          <Icon name={icon ?? 'briefcase'} size={15} color="var(--kith-muted)" />
        )}
      </span>
      <span className="cs-card-text">
        <b>{title}</b>
        <small className={ink ? 'ink' : ''}>{detail}</small>
      </span>
      {count > 0 ? <CountBadge count={count} /> : null}
    </button>
  )
}
