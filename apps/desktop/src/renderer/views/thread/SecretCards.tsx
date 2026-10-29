import { useEffect, useState } from 'react'
import type { Entry } from '@shared/models'
import { isAccountName, secretOutcome, valueProblem } from '@shared/secret-card'
import { Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { pairStyle, FALLBACK_PAIR } from '../../components/kith/pair.ts'
import type { Pair } from '../../components/kith'
import './secret-card.css'

interface Props {
  entry: Entry
  /** The agent asking. */
  agent: string
  pair?: Pair
  /** The option id being sent, or null. */
  answering: string | null
  respond: (option: string) => void
}

/**
 * Access card: the owner's accounts for a service, each with the agents that hold it. Allow grants the picked account to
 * this agent; "Another account" turns the same card into a Key card (the host updates the entry in place).
 */
export function AccessCard({ entry, agent, pair = FALLBACK_PAIR, answering, respond }: Props) {
  const d = entry.data
  const secret = d.secret!
  const accounts = d.accounts ?? []
  const [picked, setPicked] = useState<string | null>(accounts.length === 1 ? accounts[0]!.profile : null)
  const outcome = secretOutcome(secret)
  const busy = answering !== null
  return (
    <section className={`kc-decision question secret-card${outcome ? '' : ' asking'}`} style={pairStyle(pair)} aria-label={`${agent} wants ${secret.service}`}>
      <div className="kc-decision-title kith-text-body">{`${agent} wants ${secret.service}`}</div>
      {secret.purpose ? <div className="secret-card-why kith-text-secondary">{secret.purpose}</div> : null}
      {outcome ? (
        <div className="kc-decision-outcome kith-text-secondary">{outcome}</div>
      ) : (
        <>
          <div role="radiogroup" aria-label="Accounts" className="secret-card-accounts">
            {accounts.map((a) => (
              <button key={a.profile} role="radio" aria-checked={picked === a.profile} className={`secret-card-account ${picked === a.profile ? 'on' : ''}`} disabled={busy} onClick={() => setPicked(a.profile)}>
                <span className="secret-card-radio" aria-hidden />
                <span className="secret-card-account-text">
                  <span className="secret-card-account-name">{a.account}</span>
                  <span className="secret-card-account-agents kith-text-secondary">{a.agents.length ? `Used by ${a.agents.join(', ')}` : 'No agent has it yet'}</span>
                </span>
              </button>
            ))}
          </div>
          <div className="kc-decision-buttons" role="group">
            <button className="kc-decision-button ink" disabled={busy || picked === null} onClick={() => picked && respond(`profile:${picked}`)}>
              <span className="kc-decision-label">Allow</span>
              {answering === `profile:${picked}` ? <Spinner size={12} /> : null}
            </button>
            <button className="kc-decision-button pair" disabled={busy} onClick={() => respond('another')}>
              <span className="kc-decision-label">Another account</span>
              {answering === 'another' ? <Spinner size={12} /> : null}
            </button>
            <button className="kc-decision-button plain" disabled={busy} onClick={() => respond('deny')}>
              <span className="kc-decision-label">Not now</span>
              {answering === 'deny' ? <Spinner size={12} /> : null}
            </button>
          </div>
        </>
      )}
    </section>
  )
}

/**
 * Key card: one secure field per variable, and the account's name unless the card replaces a key. The typed values live in
 * this component's state alone: they are not put in the store, a draft, a pref or a log, they leave only in `save`, and they
 * are cleared when the key is saved, when the card is answered somewhere else, and when the card unmounts.
 */
export function KeyCard({ entry, agent, pair = FALLBACK_PAIR, answering, respond, save }: Props & { save: (account: string, values: Record<string, string>) => Promise<void> }) {
  const d = entry.data
  const secret = d.secret!
  const pending = secret.state === 'pending'
  const [account, setAccount] = useState('')
  const [values, setValues] = useState<Record<string, string>>({})
  const [sending, setSending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const clear = () => setValues({})
  // Closed here or on another device while typing: nothing typed stays around.
  useEffect(() => {
    if (!pending) {
      setValues({})
      setSending(false)
    }
  }, [pending])
  useEffect(() => () => setValues({}), [])

  const outcome = secretOutcome(secret)
  const label = secret.replaces ? null : account.trim()
  const accountBad = !secret.replaces && label !== '' && !isAccountName(label!)
  const complete = secret.vars.every((v) => (values[v] ?? '').length > 0)
  const canSave = pending && !sending && answering === null && complete && (secret.replaces || (label !== '' && !accountBad))

  const submit = async () => {
    if (!canSave) return
    const problem = secret.vars.map((v) => valueProblem(values[v] ?? '')).find((p) => p !== null)
    if (problem) return setError(problem)
    setSending(true)
    setError(null)
    try {
      await save(secret.replaces ? (secret.account ?? '') : label!, values)
      clear()
    } catch (e) {
      // The fields stay so a retry is one tap.
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setSending(false)
    }
  }

  return (
    <section className={`kc-decision question secret-card${outcome ? '' : ' asking'}`} style={pairStyle(pair)} aria-label={`${agent} needs a key for ${secret.service}`}>
      <div className="secret-card-head">
        <Icon name="key" size={14} />
        <div className="kc-decision-title kith-text-body">{`${secret.service} key for ${agent}`}</div>
      </div>
      {secret.purpose ? <div className="secret-card-why kith-text-secondary">{secret.purpose}</div> : null}
      {outcome ? (
        <div className="kc-decision-outcome kith-text-secondary">{outcome}</div>
      ) : (
        <form
          className="secret-card-form"
          onSubmit={(e) => {
            e.preventDefault()
            void submit()
          }}
        >
          {secret.replaces ? (
            <div className="secret-card-why kith-text-secondary">{`Replaces the key for ${secret.profile ?? secret.service}`}</div>
          ) : (
            <label className="secret-card-field">
              <span className="kith-text-secondary">Account name</span>
              <input
                className="secret-card-input"
                type="text"
                value={account}
                placeholder="work"
                autoComplete="off"
                spellCheck={false}
                autoCapitalize="off"
                autoCorrect="off"
                disabled={sending}
                aria-invalid={accountBad}
                onChange={(e) => setAccount(e.target.value)}
              />
              {accountBad ? <span className="secret-card-error">Use lowercase letters, numbers, dot, dash or underscore (up to 40).</span> : null}
            </label>
          )}
          {secret.vars.map((v) => (
            <label key={v} className="secret-card-field">
              <span className="kith-text-secondary">{v}</span>
              <input
                className="secret-card-input"
                type="password"
                name={`kith-secret-${v}`}
                value={values[v] ?? ''}
                autoComplete="off"
                spellCheck={false}
                autoCapitalize="off"
                autoCorrect="off"
                disabled={sending}
                onChange={(e) => setValues((cur) => ({ ...cur, [v]: e.target.value }))}
              />
            </label>
          ))}
          {error ? <div className="secret-card-error" role="alert">{error}</div> : null}
          <div className="secret-card-lock kith-text-secondary">
            <Icon name="key" size={10} />
            <span>Saved to the Keychain on this Mac. The agent never reads it.</span>
          </div>
          <div className="kc-decision-buttons" role="group">
            <button type="submit" className="kc-decision-button ink" disabled={!canSave}>
              <span className="kc-decision-label">Save key</span>
              {sending ? <Spinner size={12} /> : null}
            </button>
            <button type="button" className="kc-decision-button plain" disabled={sending || answering !== null} onClick={() => respond('deny')}>
              <span className="kc-decision-label">Not now</span>
              {answering === 'deny' ? <Spinner size={12} /> : null}
            </button>
          </div>
        </form>
      )}
    </section>
  )
}
