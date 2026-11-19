import type { ReactNode } from 'react'
import type { CardFact } from '../../../shared/models.ts'
import { Icon } from '../../components/Icon'
import { factNote, factPieces, factSymbol } from './permission-facts.ts'

/**
 * The top of a permission card, in the order of the three voices: what the agent says (its own description, in quotes,
 * behind a rule in its colour), what Kith sees (the host's reading of the call), and the safety check. The agent's words
 * live only here: never the headline, a button or the folded line.
 */
export function PermissionIntro({ agent, avatar, intent, facts, check, hostName }: { agent: string; avatar?: ReactNode; intent?: string; facts: CardFact[]; check?: string; hostName: string }) {
  return (
    <div className="permission-intro">
      {intent ? (
        <div className="permission-says">
          <div className="permission-says-who">
            {avatar}
            <span>{agent} says</span>
          </div>
          {/* Plain text only: React never reads markdown or links from it. */}
          <blockquote className="permission-says-quote">{`“${intent}”`}</blockquote>
        </div>
      ) : null}
      <div className="permission-sees">
        <div className="permission-sees-label">Kith sees</div>
        {facts.map((f, i) => (
          <div className="permission-fact" key={i}>
            <Icon name={factSymbol(f.icon)} size={15} />
            <span className="permission-fact-text">
              {factPieces(f).map((p, j) => (p.bold ? <strong key={j}>{p.text}</strong> : <span key={j}>{p.text}</span>))}
              {factNote(f, hostName) ? <span className="permission-fact-note">{` · ${factNote(f, hostName)}`}</span> : null}
            </span>
          </div>
        ))}
        {check ? (
          <div className="permission-check">
            <Icon name="checkmark.shield" size={14} />
            <span>{check}</span>
          </div>
        ) : null}
      </div>
    </div>
  )
}
