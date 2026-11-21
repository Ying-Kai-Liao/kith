import type { GrantedAgent } from '@shared/mac-role'
import { LEVELS_EXPLAINED, mockSharedWithYou, sharedAgentRows, type SharedWithYouProvider } from './shared-with-you'

/** What the owner shared with this account on one computer: read-only, names and levels from the host's own `hello`. */
export function SharedWithYou({ computerId, ownerName, store, provider = mockSharedWithYou }: {
  computerId: string
  ownerName: string
  store: { granted: GrantedAgent[] | null; shownConnection: { kind: string } }
  provider?: SharedWithYouProvider
}) {
  if (store.shownConnection.kind !== 'online') return null
  const rows = sharedAgentRows(store.granted ?? [])
  const rooms = provider.rooms(computerId)
  return (
    <details className="web-shared">
      <summary>Shared with you</summary>
      <p className="web-fine">From {ownerName}</p>
      {rows.length === 0 && rooms.length === 0 ? <p className="web-fine">Nothing shared with you yet.</p> : null}
      <ul>
        {rows.map((r) => (
          <li key={r.key}>
            <span>{r.name}</span>
            {r.level ? <span className="web-level">{r.level}</span> : null}
          </li>
        ))}
        {rooms.map((n) => (
          <li key={`room:${n}`}>
            <span>{n}</span>
            <span className="web-level">Room</span>
          </li>
        ))}
      </ul>
      <p className="web-fine">{LEVELS_EXPLAINED}</p>
    </details>
  )
}
