import { useCallback, useEffect, useState, type ReactNode } from 'react'
import { accountText, accessHidden, agentKeys, keyDetail, keyName } from '@shared/kith-access'
import type { KeyRow } from '@shared/secret-card'
import { Button, Spinner } from '../../../components/Controls'
import { useStore } from '../../../store/context'
import type { KithInfoStore } from '../../../store/kith-info-store'
import { AccessGrants } from './AccessGrants'
import { PermissionRow } from './HqSettings'

const message = (error: unknown) => (error instanceof Error ? error.message : String(error))

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="details-section">
      <span className="details-section-title">{title}</span>
      <div className="outlined-card kith-card">{children}</div>
    </section>
  )
}

/** The agent's keys from `listKeys`; `hidden` once the host says no (a member's 403, an older host's 404). */
function useAgentKeys(agent: string) {
  const store = useStore()
  const [keys, setKeys] = useState<KeyRow[] | null>(null)
  const [hidden, setHidden] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const load = useCallback(async () => {
    try {
      setKeys(agentKeys(await store.listKeys(), agent))
      setError(null)
    } catch (e) {
      if (accessHidden(e)) setHidden(true)
      else setError(message(e))
    }
  }, [store, agent])
  useEffect(() => void load(), [load])
  return { keys, hidden, error, load }
}

/**
 * The agent sheet's Access tab: the accounts this agent holds, its keys, when it asks first, and who else may use it.
 * Owner only; each section hides itself on a 403 or an older host's 404 and says so quietly when it has nothing.
 */
export function AccessTab({ agent, kith }: { agent: string; kith: KithInfoStore }) {
  const store = useStore()
  const { keys, hidden, error, load } = useAgentKeys(agent)
  const [removing, setRemoving] = useState<string | null>(null)
  const [removeError, setRemoveError] = useState<string | null>(null)
  const remove = async (k: KeyRow) => {
    if (removing) return
    setRemoving(k.profile)
    setRemoveError(null)
    try {
      await store.revokeKey(agent, k.service, k.profile)
      await load()
    } catch (e) {
      setRemoveError(message(e))
    }
    setRemoving(null)
  }
  const loading = keys === null && !hidden && !error
  return (
    <>
      {!hidden ? (
        <Section title="Accounts">
          {loading ? <p className="kith-muted kith-loading" role="status"><Spinner size={12} /> Loading…</p>
            : error ? <p className="kith-muted kith-failed" role="alert">Couldn't read the accounts: {error}</p>
            : !keys?.length ? <p className="kith-muted">No accounts held</p>
            : keys.map((k) => (
              <div key={k.profile} className="kith-model kith-access-row">
                <div className="kith-access-text"><span className="kith-ellipsis" title={k.profile}>{accountText(k)}</span></div>
                <Button kind="secondary" disabled={removing !== null} onClick={() => void remove(k)}>{removing === k.profile ? 'Removing…' : 'Remove'}</Button>
              </div>
            ))}
          {removeError ? <span className="kith-failed" role="alert">Not removed: {removeError}</span> : null}
        </Section>
      ) : null}
      {!hidden && keys?.length ? (
        <Section title="Keys">
          {keys.map((k) => (
            <div key={k.profile} className="kith-model kith-access-row">
              <div className="kith-access-text">
                <span className="kith-ellipsis" title={keyName(k)}>{keyName(k)}</span>
                <span className="kith-muted kith-ellipsis">{keyDetail(k)}</span>
              </div>
            </div>
          ))}
        </Section>
      ) : null}
      {kith.info ? (
        <Section title="Asks before">
          <PermissionRow kith={kith} />
        </Section>
      ) : null}
      <AccessGrants agent={agent} title="People" />
    </>
  )
}
