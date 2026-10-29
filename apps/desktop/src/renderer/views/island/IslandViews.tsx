import { useRef, useState } from 'react'
import type { IslandBot, IslandPermission } from '@shared/island'
import { Button, Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { CountBadge } from '../../components/kith'
import { islandOptionLabel } from '../thread/permission-labels.ts'
import { ThinkingOrb } from '../../components/ThinkingOrb'
import { avatarColor, withAlpha } from '../../lib/theme'
import type { IslandModel } from './island-model'
import { useArrivals, useFlip, useSwap } from './island-motion'
import { AgentRow, badgeOf, BigCharacter, GREEN, IslandBotAvatar, KeyHint, MiniColumn, moodOf, RED, StateBadge, StatusDot, stepIcon } from './island-parts'

const veil = (color: string, alpha: number) => `radial-gradient(120% 90% at 50% 130%, ${withAlpha(color, alpha)}, ${withAlpha(color, 0)} 70%)`

/**
 * Closed over the notch: the bot in focus on the left wing; the others (or its state) on the
 * right. With no notch to hide, the middle says what's going on.
 */
export function Wings({ model, peek }: { model: IslandModel; peek: boolean }) {
  const focus = model.focus
  const others = model.others(4)
  const swap = useSwap(focus?.ref)
  const arrivals = useArrivals(others.map((b) => b.ref))
  const cluster = useRef<HTMLSpanElement>(null)
  useFlip(cluster)
  if (!focus) return null
  const label = focus.status === 'needsInput' ? 'Needs you' : `${model.activeCount} working`
  return (
    <div className={`island-wings ${peek ? 'peeking' : ''}`}>
      {model.screen.notch || peek ? null : <span className="island-wings-label">{label}</span>}
      <span className="island-wing-left">
        <span key={focus.ref} className={`island-wing-avatar ${swap}`}>
          <IslandBotAvatar bot={focus} size={peek ? 18 : 20} />
          {peek ? null : <StateBadge badge={badgeOf(focus)} small />}
        </span>
      </span>
      {peek ? null : (
        <span className="island-wing-right">
          {others.length ? (
            <span ref={cluster} className="island-cluster">
              {others.map((b) => (
                <span key={b.ref} data-flip={b.ref} className={`island-cluster-bot ${arrivals.has(b.ref) ? 'island-swap' : ''}`}>
                  <IslandBotAvatar bot={b} size={11} />
                  {b.status === 'needsInput' ? <StatusDot bot={b} /> : null}
                </span>
              ))}
            </span>
          ) : focus.status === 'needsInput' ? (
            <CountBadge inverted count={model.summary.bots.filter((b) => b.status === 'needsInput').length || 1} size={16} label="Needs you" />
          ) : (
            <ThinkingOrb size={14} color="#F5F6F8" />
          )}
        </span>
      )}
    </div>
  )
}

/** The bot in focus with its latest steps, and the other bots as pills. */
export function Overview({ model }: { model: IslandModel }) {
  const focus = model.focus
  const others = model.others(4)
  const swap = useSwap(focus?.ref)
  const arrivals = useArrivals(others.map((b) => b.ref))
  const pills = useRef<HTMLDivElement>(null)
  useFlip(pills)
  if (!focus) return null
  const steps = focus.steps.length ? focus.steps : [{ title: focus.detail, kind: 'other' }]
  return (
    <div className="island-row-cards">
      <button className={`island-card island-focus ${others.length ? '' : 'alone'}`} onClick={() => model.openBot(focus.ref)} title="Open conversation">
        <span key={focus.ref} className={`island-swap-slot ${swap}`}>
          <BigCharacter bot={focus} size={70} mood={moodOf(focus)} badge={badgeOf(focus)} />
          <span className="island-focus-text">
            <AgentRow color={focus.color} name={focus.name} subtitle={focus.backend} />
            {steps.map((step, i) => (
              <span key={`${i}-${step.title}`} className={`island-step ${i === steps.length - 1 ? 'current' : ''}`}>
                <Icon name={stepIcon(step.kind)} size={i === steps.length - 1 ? 13 : 12} scaled={false} />
                <span className="truncate">{step.title}</span>
              </span>
            ))}
          </span>
        </span>
      </button>
      {others.length ? (
        <div ref={pills} className="island-card island-pills">
          {others.map((b) => (
            <button
              key={b.ref}
              data-flip={b.ref}
              className={`island-pill ${arrivals.has(b.ref) ? 'island-swap' : ''}`}
              style={{ background: withAlpha(avatarColor(b.color), 0.16), color: `color-mix(in srgb, ${avatarColor(b.color)} 55%, white)` }}
              onClick={() => model.openBot(b.ref)}
              title={b.detail}
            >
              <span className="island-pill-bot">
                <IslandBotAvatar bot={b} size={24} />
                <StatusDot bot={b} />
              </span>
              <span className="truncate">{b.name}</span>
            </button>
          ))}
        </div>
      ) : null}
    </div>
  )
}

export function Empty({ model }: { model: IslandModel }) {
  const focus = model.focus
  return (
    <div className="island-card island-empty">
      {focus ? <BigCharacter bot={focus} size={62} mood="idle" badge={null} /> : null}
      <span className="island-empty-text">
        <span className="island-title">Nothing running right now.</span>
        <span className="island-subtitle">{focus ? 'Your crew is idle. Message an agent from here, or open Kith.' : 'Make an agent in Kith to get started.'}</span>
      </span>
      {focus ? <Button onClick={() => model.setTab('message')}>Message an agent</Button> : <Button onClick={() => model.newBot()}>New agent</Button>}
    </div>
  )
}

/** Deny on the left, the main allow on the right. */
const optionOrder = (kind: string) => ({ reject_always: 0, reject_once: 1, allow_always: 2, allow_once: 3 })[kind] ?? 1.5

export function Approval({ model, card }: { model: IslandModel; card: IslandPermission }) {
  const options = [...card.options].sort((a, b) => optionOrder(a.kind) - optionOrder(b.kind))
  const swap = useSwap(card.entryId)
  return (
    <div className="island-with-minis">
      <div className="island-card island-alert" style={{ background: `${veil(avatarColor(card.bot.color), 0.3)}, var(--island-card)` }}>
        <div key={card.entryId} className={`island-swap-slot ${swap}`}>
          <BigCharacter bot={card.bot} size={56} mood="needsInput" badge="needsInput" onOpen={() => model.openBot(card.ref)} />
          <div className="island-alert-text">
            <AgentRow color={card.bot.color} name={card.bot.name} subtitle={`${card.bot.backend} ${card.headline.charAt(0).toLowerCase()}${card.headline.slice(1)}`} />
            {card.title ? <div className="island-code">{card.title}</div> : null}
            <div className="island-buttons">
              {/* A key request has no answer to give here: the field is in the chat. */}
              {options.length === 0 ? <Button onClick={() => model.openBot(card.ref)}>Open</Button> : null}
              {options.map((o) => (
                <Button key={o.id} kind={o.kind === 'allow_once' ? 'primary' : 'secondary'} className={o.kind.startsWith('reject') ? 'island-deny' : undefined} disabled={card.answering !== null} onClick={() => model.respond(card, o.id)}>
                  {card.answering === o.id ? <Spinner size={12} /> : null}
                  {islandOptionLabel(o.kind, o.label)}
                  {o.kind === 'allow_once' ? <KeyHint keys="⌥Y" /> : o.kind === 'reject_once' ? <KeyHint keys="⌥N" /> : null}
                </Button>
              ))}
            </div>
          </div>
        </div>
      </div>
      <MiniColumn bots={model.others(3).filter((b) => b.ref !== card.ref)} onOpen={(ref) => model.openBot(ref)} />
    </div>
  )
}

/** A bot that just finished (for a moment) or failed (until dismissed). */
export function Notice({ model, bot, failed }: { model: IslandModel; bot: IslandBot; failed: boolean }) {
  const swap = useSwap(`${failed}:${bot.ref}`)
  return (
    <div className="island-with-minis">
      <div className="island-card island-alert" style={{ background: `${veil(failed ? RED : GREEN, failed ? 0.5 : 0.42)}, var(--island-card)` }}>
        <div key={`${failed}:${bot.ref}`} className={`island-swap-slot ${swap}`}>
          <BigCharacter bot={bot} size={58} mood="idle" badge={failed ? 'failed' : 'finished'} onOpen={() => model.openBot(bot.ref)} />
          <div className="island-alert-text">
            <AgentRow color={bot.color} name={bot.name} subtitle={failed ? bot.backend : `${bot.backend} is done`} />
            {failed ? (
              <>
                <div className="island-title">{`${bot.name} stopped.`}</div>
                <div className="island-error">{bot.error ?? bot.detail}</div>
              </>
            ) : (
              <div className="island-title truncate">{bot.detail}</div>
            )}
            <div className="island-buttons start">
              <Button onClick={() => model.openBot(bot.ref)}>Open conversation</Button>
              <Button kind="secondary" onClick={() => model.dismissNotice()}>OK</Button>
            </div>
          </div>
        </div>
      </div>
      <MiniColumn bots={model.others(3).filter((b) => b.ref !== bot.ref)} onOpen={(ref) => model.openBot(ref)} />
    </div>
  )
}

/** Writes to a bot without opening Kith: the chip picks the bot, Enter sends, Esc closes. */
export function Message({ model }: { model: IslandModel }) {
  const [text, setText] = useState('')
  const bot = model.messageBot
  const swap = useSwap(bot?.ref)
  if (!bot) return null
  return (
    <div className="island-with-minis">
      <div className="island-card island-message">
        <span key={bot.ref} className={`island-message-bot ${swap}`}>
          <IslandBotAvatar bot={bot} size={44} />
        </span>
        <div className="island-message-body">
          <div className="island-agent">
            <button
              key={bot.ref}
              className={`island-chip ${swap}`}
              style={{ background: withAlpha(avatarColor(bot.color), 0.16), color: `color-mix(in srgb, ${avatarColor(bot.color)} 55%, white)` }}
              onClick={() => model.nextMessageBot()}
              title="Choose a bot"
            >
              <IslandBotAvatar bot={bot} size={18} />
              {bot.name}
              <Icon name="chevron.down" size={8} weight="bold" scaled={false} />
            </button>
            <span className="island-agent-subtitle">{`${bot.backend} · ${bot.folder}`}</span>
          </div>
          <form
            className="island-field"
            onSubmit={(e) => {
              e.preventDefault()
              model.send(text)
              setText('')
            }}
          >
            <input
              autoFocus
              aria-label={`Message ${bot.name}`}
              placeholder={`Message ${bot.name}…`}
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Escape') model.close()
              }}
            />
            <button type="submit" className="island-send" aria-label="Send" title="Send" disabled={!text.trim()}>
              <Icon name="arrow.up" size={12} weight="bold" scaled={false} />
            </button>
          </form>
        </div>
      </div>
      <MiniColumn bots={model.others(3).filter((b) => b.ref !== bot.ref)} onOpen={(ref) => model.openBot(ref)} />
    </div>
  )
}
