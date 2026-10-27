import type { Entry } from '../../../shared/models.ts'

/** What became of an answered, cancelled or expired permission card. The host's own words (`outcome`) win: an "always"
 *  that kith-auth couldn't keep stayed a once grant, and the card says so rather than "Always allowed". */
export function permissionOutcome(entry: Pick<Entry, 'data'>): string {
  const d = entry.data
  switch (d.status) {
    case 'answered': {
      if (d.outcome) return d.outcome
      const chosen = d.options?.find((o) => o.optionId === d.selected)
      switch (chosen?.kind) {
        case 'allow_once':
          return 'Allowed once'
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
