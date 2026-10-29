import { useEffect, useState } from 'react'
import { accessHidden, groupGrants, personTitle } from '@shared/kith-access'
import { accessGrantText, type AccessGrant } from '@shared/kith-info'
import type { AgentPeople } from '@shared/usage-limits'
import { Button, Spinner } from '../../../components/Controls'
import { useStore, useStoreRef } from '../../../store/context'
import { spendLine } from '../../usage/PeopleBudgets'

const message = (error: unknown) => (error instanceof Error ? error.message : String(error))

/**
 * Who may use this agent (`accessLedger {agent}`): each person with what they may reach beyond the box or use as their
 * own account in every run, a Remove that takes one away before their next run, and their spend and limit here
 * (`agentPeople`, when the host has it). A member (403) or an older host (404) sees nothing.
 */
export function AccessGrants({ agent, title = 'People' }: { agent: string; title?: string }) {
  const host = useStoreRef()
  const store = useStore()
  const [grants, setGrants] = useState<AccessGrant[] | null>(null)
  const [hidden, setHidden] = useState(false)
  const [people, setPeople] = useState<AgentPeople | null>(null)
  const [removing, setRemoving] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let live = true
    setGrants(null)
    host.ready().then((c) => c.accessLedger({ agent })).then((g) => live && setGrants(g)).catch((e: unknown) => {
      if (!live) return
      if (accessHidden(e)) setHidden(true)
      else setError(message(e))
    })
    // The budgets are a bonus: without them the list stands.
    host.ready().then((c) => c.agentPeople(agent)).then((p) => live && setPeople(p)).catch(() => {})
    return () => { live = false }
  }, [host, agent])
  if (hidden || !store.isOwner) return null
  const remove = async (g: AccessGrant) => {
    if (removing) return
    setRemoving(g.key)
    setError(null)
    try {
      await (await host.ready()).removeGrant(g)
      setGrants((now) => (now ?? []).filter((x) => x.key !== g.key))
    } catch (e) {
      setError(message(e))
    }
    setRemoving(null)
  }
  const groups = groupGrants(grants ?? [])
  return (
    <section className="details-section">
      <span className="details-section-title">{title}</span>
      <div className="outlined-card kith-card">
        {grants === null && !error ? <p className="kith-muted kith-loading" role="status"><Spinner size={12} /> Loading…</p> : null}
        {grants && !groups.length ? <p className="kith-muted">Only you use this agent</p> : null}
        {groups.map((p) => {
          const spend = p.person === '*' ? null : spendLine(people, p.person)
          return (
            <div key={p.person} className="kith-access-person">
              <strong className="kith-ellipsis">{personTitle(p)}</strong>
              {spend ? <span className="kith-muted">{spend}</span> : null}
              {p.grants.map((g) => (
                <div key={g.key} className="kith-model kith-access-row">
                  <div className="kith-access-text">
                    <span className="kith-ellipsis" title={g.target}>{accessGrantText(g)}</span>
                    {g.since !== null ? <span className="kith-muted">since {new Date(g.since).toLocaleDateString([], { day: 'numeric', month: 'short', year: 'numeric' })}</span> : null}
                  </div>
                  <Button kind="secondary" disabled={removing !== null} onClick={() => void remove(g)}>{removing === g.key ? 'Removing…' : 'Remove'}</Button>
                </div>
              ))}
            </div>
          )
        })}
        {error ? <span className="kith-failed" role="alert">{grants ? 'Not removed' : "Couldn't read this"}: {error}</span> : null}
        {groups.length ? <small className="kith-muted">Kept until removed. A removed one is gone from their next run.</small> : null}
      </div>
    </section>
  )
}
