import type { ReactNode } from 'react'
import type { Entry } from '@shared/models'
import { limitCardModel, limitOutcome } from '@shared/usage-limits'
import { Spinner } from '../../components/Controls'
import { pairStyle, type Pair } from '../../components/kith/pair.ts'
import { LimitBar } from '../usage/SpendSection'
import './limit-card.css'

/**
 * An agent at its monthly limit (the owner's run waits under it), or a guest at their budget: the host's card
 * (serve/host/src/limits.rs), tinted in the agent's pair. The words are limitCardModel's; the answer is the store's.
 * `waiting`: someone else answers it (a guest's own view); the buttons give way to that line.
 */
export function LimitCard({ entry, pair, answering, respond, leading, time, waiting = null, personName, board = false }: {
  entry: Entry
  pair: Pair
  answering: string | null
  respond: (option: string) => void
  leading?: ReactNode
  time?: string
  waiting?: string | null
  personName?: string
  /** Shown on Board, away from its chat. */
  board?: boolean
}) {
  const m = limitCardModel(entry, personName, board)
  if (!m) return null
  const pending = entry.data.status === 'pending'
  const main = m.options.filter((o) => o.tone !== 'plain')
  const quiet = m.options.filter((o) => o.tone === 'plain')
  const button = (o: (typeof m.options)[number]) => (
    <button key={o.id} className={`lc-button ${o.tone}`} disabled={answering !== null} onClick={() => respond(o.id)}>
      <span>{o.label}</span>
      {answering === o.id ? <Spinner size={12} /> : null}
    </button>
  )
  return (
    <div className="lc-wrap">
    <section className="lc" style={pairStyle(pair)} aria-label={m.heading}>
      <div className="lc-heading">
        {leading ? <span className="lc-leading">{leading}</span> : null}
        <span className="lc-heading-text">{m.heading}</span>
        {time ? <span className="lc-time">{time}</span> : null}
      </div>
      <div className="lc-title">{m.title}</div>
      <div className="lc-line">
        <span>{m.left}</span>
        <span className="lc-amount">{m.right}</span>
      </div>
      <LimitBar value={m.fullness} pair={{ a: 'var(--kith-ink)', b: 'var(--kith-ink)' }} label={m.right} />
      {!pending ? (
        <div className="lc-outcome">{limitOutcome(entry)}</div>
      ) : waiting ? (
        <div className="lc-outcome">{`Waiting for ${waiting}`}</div>
      ) : (
        <div className="lc-buttons" role="group">
          {main.map(button)}
          <span className="lc-spacer" />
          {quiet.map(button)}
        </div>
      )}
    </section>
    {m.footer && pending && !waiting ? <div className="lc-footer">{m.footer}</div> : null}
    </div>
  )
}
