import { Icon } from '../../components/Icon'
import type { Fold } from './fold-card'

/** An answered or finished card as one pill line: a green check (red x when denied), what happened, the time. */
export function FoldedPill({ fold, time, open, toggle }: { fold: Fold; time: string; open: boolean; toggle: () => void }) {
  return (
    <button className="folded-pill press" aria-expanded={open} title={open ? 'Fold the card' : 'Show the card'} onClick={toggle}>
      <Icon name={fold.ok ? 'checkmark.circle.fill' : 'xmark.circle.fill'} size={14} color={fold.ok ? 'var(--kith-positive)' : 'var(--kith-negative)'} />
      <span className="folded-pill-text">{fold.text}</span>
      <span className="folded-pill-time">{time}</span>
    </button>
  )
}
