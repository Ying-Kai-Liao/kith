import type { Entry, SessionGrant } from '../../../shared/models.ts'

const hhmm = (ms: number) => {
  const at = new Date(ms)
  return `${String(at.getHours()).padStart(2, '0')}:${String(at.getMinutes()).padStart(2, '0')}`
}

const ENDED: Record<string, string> = { task: 'ended with the task', expired: 'ended after 24 h', settings: 'ended, settings changed' }

/** The outcome line of a card answered with This session: "Allowed this session", then how it stopped once it has. */
export function sessionOutcome(g: SessionGrant): string {
  if (g.state === 'removed') return `Allowed this session · ${g.endedAt ? `removed ${hhmm(g.endedAt)}` : 'removed'}`
  if (g.state === 'ended') return `Allowed this session · ${ENDED[g.endReason ?? ''] ?? 'ended'}`
  return 'Allowed this session'
}

/** What became of an answered, cancelled or expired permission card. The host's own words (`outcome`) win: an "always"
 *  that kith-auth couldn't keep stayed a once grant, and the card says so rather than "Always allowed". A This session
 *  grant goes by its state, which the host keeps current. */
export function permissionOutcome(entry: Pick<Entry, 'data'>): string {
  const d = entry.data
  switch (d.status) {
    case 'answered': {
      if (d.sessionGrant) return sessionOutcome(d.sessionGrant)
      if (d.outcome) return d.outcome
      const chosen = d.options?.find((o) => o.optionId === d.selected)
      switch (chosen?.kind) {
        case 'allow_once':
          return 'Allowed once'
        case 'allow_session':
          return 'Allowed this session'
        case 'allow_always':
          return 'Always allowed'
        case 'reject_once':
          return 'Denied'
        case 'reject_always':
          return 'Never allowed'
        default:
          return chosen?.name ?? 'Answered'
      }
    }
    case 'cancelled':
      return 'Cancelled'
    default:
      return 'Expired — the agent moved on'
  }
}
