import { useEffect, useState } from 'react'
import { IconButton, Spinner } from '../../../components/Controls'
import { useStore } from '../../../store/context'
import { connectorStatusText, removableSkill, type KithTools } from '@shared/kith-tools'
import type { HostClient } from '../../../client/host-client'

/**
 * The connectors and skills a kith agent holds, as `kith <agent> connectors|skills --json` says, each with Remove.
 * Adding happens in the Marketplace, which asks which bot. Read again on every change, never patched locally.
 */
export function AgentTools({ botId }: { botId: string }) {
  const store = useStore()
  const [tools, setTools] = useState<KithTools | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)

  const load = async () => {
    try {
      setTools(await (await store.ready()).kithTools(botId))
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }
  useEffect(() => {
    void load()
  }, [botId])

  const remove = async (key: string, op: (c: HostClient) => Promise<unknown>) => {
    if (busy) return
    setBusy(key)
    setError(null)
    try {
      await op(await store.ready())
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
    await load()
    setBusy(null)
  }

  const failed = (what: string, why: string | null) => (why ? <p className="kith-muted kith-failed" role="alert">Couldn't read {what}: {why}</p> : null)
  const connectors = tools?.connectors.filter((c) => c.status !== 'not granted') ?? []
  return (
    <section className="details-section">
      <span className="details-section-title">Connectors and skills</span>
      <div className="outlined-card kith-card">
        {!tools && !error ? <p className="kith-muted kith-loading" role="status"><Spinner size={12} /> Reading…</p> : null}
        {error ? <p className="kith-muted kith-failed" role="alert">{error}</p> : null}
        {tools ? (
          <>
            {failed('connectors', tools.connectorsError)}
            {failed('skills', tools.skillsError)}
            {connectors.map((c) => (
              <div key={`c:${c.name}`} className="kith-item" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <strong className="kith-ellipsis">{c.name}</strong>
                  <p className="kith-muted kith-ellipsis">{connectorStatusText(c)}{c.declared ? ' · part of its package' : ''}</p>
                </div>
                {busy === `c:${c.name}` ? <Spinner size={12} /> : (
                  <IconButton title={`Remove ${c.name}`} icon="trash" onClick={() => void remove(`c:${c.name}`, (client) => client.kithDisconnect(botId, c.name))} />
                )}
              </div>
            ))}
            {tools.skills.map((s) => (
              <div key={`s:${s.name}`} className="kith-item" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <strong className="kith-ellipsis">{s.name}</strong>
                  <p className="kith-muted kith-ellipsis" title={s.description}>{s.kind === 'linked' ? 'From GitHub' : s.kind === 'learned' ? 'Learned' : 'Its own'}{s.description ? ` · ${s.description}` : ''}</p>
                </div>
                {!removableSkill(s) ? null : busy === `s:${s.name}` ? <Spinner size={12} /> : (
                  <IconButton title={`Remove ${s.name}`} icon="trash" onClick={() => void remove(`s:${s.name}`, (client) => client.kithSkillRemove(botId, s.name))} />
                )}
              </div>
            ))}
            {!connectors.length && !tools.skills.length && !tools.connectorsError && !tools.skillsError
              ? <p className="kith-muted">None yet. Add some from Extensions.</p> : null}
          </>
        ) : null}
      </div>
    </section>
  )
}
