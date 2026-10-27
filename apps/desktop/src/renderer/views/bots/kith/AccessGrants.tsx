import { useEffect, useState } from 'react'
import { accessGrantHolder, accessGrantText, type AccessGrant } from '@shared/kith-info'
import { Button } from '../../../components/Controls'
import { useStoreRef } from '../../../store/context'

const message = (error: unknown) => (error instanceof Error ? error.message : String(error))
const key = (g: AccessGrant) => `${g.person}\n${g.kind}\n${g.target}`

/**
 * The agent's "always" answers to escape cards (`kithAccessGrants`): what each person may reach beyond the box in every
 * run, with a Remove that takes one away before their next run. Shown only when there is one; a member (403) or an older
 * host (404) sees nothing.
 */
export function AccessGrants({ botId }: { botId: string }) {
  const host = useStoreRef()
  const [grants, setGrants] = useState<AccessGrant[]>([])
  const [removing, setRemoving] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let live = true
    host.ready().then((c) => c.kithAccessGrants(botId)).then((g) => live && setGrants(g)).catch(() => {})
    return () => { live = false }
  }, [host, botId])
  if (!grants.length) return null
  const remove = async (g: AccessGrant) => {
    setRemoving(key(g))
    setError(null)
    try {
      setGrants(await (await host.ready()).kithRemoveAccessGrant(botId, g))
    } catch (e) {
      setError(message(e))
    }
    setRemoving(null)
  }
  return (
    <section className="details-section">
      <span className="details-section-title">Access</span>
      <div className="outlined-card kith-card">
        {grants.map((g) => (
          <div key={key(g)} className="kith-model kith-access-row">
            <div className="kith-access-text">
              <span className="kith-ellipsis" title={g.target}>{accessGrantText(g)}</span>
              <span className="kith-muted kith-ellipsis">{accessGrantHolder(g)}</span>
            </div>
            <Button kind="secondary" disabled={removing !== null} onClick={() => void remove(g)}>{removing === key(g) ? 'Removing…' : 'Remove'}</Button>
          </div>
        ))}
        {error ? <span className="kith-failed" role="alert">Not removed: {error}</span> : null}
        <small className="kith-muted">Allowed always from a permission card. A removed one is gone from their next run.</small>
      </div>
    </section>
  )
}
