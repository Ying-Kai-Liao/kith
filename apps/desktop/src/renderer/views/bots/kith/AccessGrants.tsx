import { useEffect, useState } from 'react'
import { accessGrantHolder, accessGrantText, type AccessGrant } from '@shared/kith-info'
import { Button } from '../../../components/Controls'
import { useStoreRef } from '../../../store/context'

const message = (error: unknown) => (error instanceof Error ? error.message : String(error))

/**
 * The agent's part of the grants ledger (`accessLedger {agent}`): what each person may reach beyond the box, or use as
 * their own account, in every run, with a Remove that takes one away before their next run. Shown only when there is
 * one; a member (403) or an older host (404) sees nothing.
 */
export function AccessGrants({ agent }: { agent: string }) {
  const host = useStoreRef()
  const [grants, setGrants] = useState<AccessGrant[]>([])
  const [removing, setRemoving] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let live = true
    host.ready().then((c) => c.accessLedger({ agent })).then((g) => live && setGrants(g)).catch(() => {})
    return () => { live = false }
  }, [host, agent])
  if (!grants.length) return null
  const remove = async (g: AccessGrant) => {
    setRemoving(g.key)
    setError(null)
    try {
      await (await host.ready()).removeGrant(g)
      setGrants((now) => now.filter((x) => x.key !== g.key))
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
          <div key={g.key} className="kith-model kith-access-row">
            <div className="kith-access-text">
              <span className="kith-ellipsis" title={g.target}>{accessGrantText(g)}</span>
              <span className="kith-muted kith-ellipsis">{accessGrantHolder(g)}</span>
            </div>
            <Button kind="secondary" disabled={removing !== null} onClick={() => void remove(g)}>{removing === g.key ? 'Removing…' : 'Remove'}</Button>
          </div>
        ))}
        {error ? <span className="kith-failed" role="alert">Not removed: {error}</span> : null}
        <small className="kith-muted">Kept until removed. A removed one is gone from their next run.</small>
      </div>
    </section>
  )
}
