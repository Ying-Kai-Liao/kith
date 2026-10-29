import type { ReactNode } from 'react'
import { Spinner } from '../Controls'
import { Icon } from '../Icon'
import { pairStyle, type Pair } from './pair.ts'
import './kith.css'

export interface DecisionOption {
  id: string
  label: string
}

interface Common {
  /** A muted label above the title: whose it is ("Sam's bot · Grok"). */
  heading?: string
  /** A picture before the heading (an outside bot's avatar). */
  leading?: ReactNode
  /** The agent's colour pair: the question buttons tint with it at low strength. */
  pair: Pair
  /** The card's headline: the question, or what the agent wants ("Wants to run a command"). */
  title: string
  /** A mono line under the title (the command or path), clamped to three lines. */
  subject?: string
  /** Small grey line, e.g. "Runs on Mac mini · kith". */
  place?: string
  /** Room for a details disclosure or a diff; the host owns it. */
  details?: ReactNode
  /** The card was answered or expired: the outcome replaces the buttons. */
  outcome?: string
  /** An old run still holds the session this approved card resumes in: the held row, under the outcome. */
  held?: ReactNode
  /** A guest's prompt that is the owner's to answer: the buttons give way to "Waiting for <name>". */
  waiting?: string | null
  /** The option id being sent (or true for permission buttons): all buttons disable, that one shows a spinner. */
  pending?: string | boolean | null
}

export type DecisionCardProps =
  | (Common & { kind: 'question'; options: DecisionOption[]; onChoose: (id: string) => void })
  | (Common & { kind: 'permission'; onAllow: () => void; onDeny: () => void; onAlways?: () => void; /** The allow button's words ("Allow" by default). */ allowLabel?: string; /** The always button's words ("Always" by default). */ alwaysLabel?: string })

/** A question waiting on you, or a permission prompt. While it waits the card is tinted in the agent's pair (grey once answered); no amber. */
export function DecisionCard(props: DecisionCardProps) {
  const { pair, heading, leading, title, subject, place, details, outcome, held, waiting, pending = null } = props
  const busy = pending !== null && pending !== false
  const buttons =
    props.kind === 'question'
      ? props.options.slice(0, 4).map((o) => ({ id: o.id, label: o.label, tone: 'pair' as const, run: () => props.onChoose(o.id) }))
      : [
          { id: 'allow', label: props.allowLabel ?? 'Allow', tone: 'ink' as const, run: props.onAllow },
          { id: 'deny', label: 'Deny', tone: 'deny' as const, run: props.onDeny },
          ...(props.onAlways ? [{ id: 'always', label: props.alwaysLabel ?? 'Always', tone: 'plain' as const, run: props.onAlways }] : []),
        ]
  return (
    <section className={`kc-decision ${props.kind}${outcome ? '' : ' asking'}`} style={pairStyle(pair)} aria-label={title}>
      {heading ? (
        <div className="kc-decision-heading kith-text-secondary">
          {leading ? <span className="kc-decision-leading">{leading}</span> : null}
          {heading}
        </div>
      ) : null}
      <div className="kc-decision-title kith-text-body">{title}</div>
      {subject ? <div className="kc-decision-subject kith-text-code">{subject}</div> : null}
      {place ? (
        <div className="kc-decision-place kith-text-secondary">
          <Icon name="desktopcomputer" size={10} />
          <span>{place}</span>
        </div>
      ) : null}
      {details}
      {outcome ? (
        <>
          <div className="kc-decision-outcome kith-text-secondary">{held ? `${outcome} · waiting to go on` : outcome}</div>
          {held}
        </>
      ) : waiting ? (
        <div className="kc-decision-outcome kith-text-secondary">{`Waiting for ${waiting}`}</div>
      ) : (
        <div className="kc-decision-buttons" role="group">
          {buttons.map((b) => (
            <button key={b.id} className={`kc-decision-button ${b.tone}`} disabled={busy} onClick={b.run}>
              <span className="kc-decision-label">{b.label}</span>
              {pending === b.id ? <Spinner size={12} /> : null}
            </button>
          ))}
        </div>
      )}
    </section>
  )
}
