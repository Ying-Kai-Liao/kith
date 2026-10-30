import type { EntryData, PermissionOption } from '../../../shared/models.ts'

const rank: Record<string, number> = { allow_once: 0, allow_always: 1, reject_once: 2, reject_always: 3 }

/** Allow before deny, once before always. */
export const sortedOptions = (options: PermissionOption[]) => [...options].sort((a, b) => (rank[a.kind] ?? 9) - (rank[b.kind] ?? 9))

/**
 * An option's button text. An edge card (`edge`: a guest's run reaching a host or helper) is labelled in the host's
 * words, since its two allow_always ("Always for Sam", "Turn on <job>") grant different things. An agent's own card
 * goes by kind.
 */
export function optionLabel(o: PermissionOption, edge?: EntryData['edge']) {
  if (edge && o.name) return o.name
  switch (o.kind) {
    case 'allow_once':
      return 'Allow once'
    case 'allow_always':
      return 'Always allow'
    case 'reject_once':
      return 'Deny'
    case 'reject_always':
      return 'Never'
    default:
      return o.name
  }
}

/**
 * The card's answers as their own buttons, or null for the plain Allow / Deny / Always card: when the host offers
 * exactly those and the card is not an edge card.
 */
export function permissionChoices(d: Pick<EntryData, 'options' | 'edge'>): { id: string; label: string }[] | null {
  const options = sortedOptions(d.options ?? [])
  const has = (kind: string) => options.some((o) => o.kind === kind)
  const plain = !d.edge && options.length > 0 && options.every((o) => ['allow_once', 'allow_always', 'reject_once'].includes(o.kind)) && has('allow_once') && has('reject_once')
  return plain ? null : options.map((o) => ({ id: o.optionId, label: optionLabel(o, d.edge) }))
}
