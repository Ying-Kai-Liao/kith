import type { Entry } from '../../../shared/models.ts'
import { isSecretCard } from '../../../shared/secret-card.ts'
import { limitOf, limitOutcome } from '../../../shared/usage-limits.ts'
import { permissionOutcome } from './permission-outcome.ts'

export interface Fold {
  /** A green check; false is a red x (denied, not now, expired). */
  ok: boolean
  text: string
}

/** The one pill line an answered or finished card shrinks to ("Allowed npm test"); null while it is pending. */
export function foldOf(entry: Entry): Fold | null {
  const d = entry.data
  if (isSecretCard(entry)) {
    const s = d.secret!
    const kind = d.card === 'account' ? 'access' : 'key'
    if (s.state === 'pending') return null
    if (s.state === 'saved') return { ok: true, text: d.card === 'account' ? `Allowed ${s.service}${s.account ? ` · ${s.account}` : ''}` : `Saved the key for ${s.profile ?? s.service}` }
    if (s.state === 'denied') return { ok: false, text: `Not now · ${s.service} ${kind}` }
    return { ok: false, text: `Expired · ${s.service} ${kind}` }
  }
  if (d.status === 'pending' || !d.status) return null
  if (limitOf(entry)) return { ok: !(d.status === 'cancelled' || d.selected === 'stop'), text: limitOutcome(entry) }
  const what = d.title ?? ''
  const chosen = d.options?.find((o) => o.optionId === d.selected)
  if (d.status === 'answered') {
    switch (chosen?.kind) {
      case 'allow_once':
        return { ok: true, text: `Allowed ${what}`.trim() }
      case 'allow_always':
        return { ok: true, text: `Always allowed ${what}`.trim() }
      case 'reject_once':
        return { ok: false, text: `Denied ${what}`.trim() }
      case 'reject_always':
        return { ok: false, text: `Never allowed ${what}`.trim() }
    }
  }
  const outcome = permissionOutcome(entry)
  return { ok: d.status === 'answered', text: what ? `${outcome} · ${what}` : outcome }
}
