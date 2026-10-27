import { useEffect, useState } from 'react'
import { BUDGET_PRESETS, fullness, money, ofLimit, personSpend, type AgentPeople } from '@shared/usage-limits'
import { isUnsupportedHost } from '@shared/kith-info'
import { CardSection } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { pairOf } from '../../components/kith/pair.ts'
import { VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import type { BotStore } from '../../store/bot-store'
import { agentBot, LimitBar } from './SpendSection'
import './spend.css'

/** What each person spent on one agent this month, and their limit: loaded once per sheet, written back by the owner. */
export function useAgentPeople(store: BotStore, agent: string) {
  const [people, setPeople] = useState<AgentPeople | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    if (!agent || !store.isOwner) return
    let live = true
    void store.ready().then((c) => c.agentPeople(agent)).then(
      (p) => { if (live) setPeople(p) },
      (e: unknown) => { if (live && !isUnsupportedHost(e)) setError(e instanceof Error ? e.message : String(e)) },
    )
    return () => { live = false }
  }, [store, agent])
  const setBudget = async (person: string, budget: number | null) => {
    setError(null)
    try {
      setPeople(await (await store.ready()).setPersonBudget(agent, person, budget))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }
  return { people, error, setBudget }
}

/** A person's line on this agent: "$20.10 of $20", or what they spent when there is no limit. */
export function spendLine(people: AgentPeople | null, personId: string): string | null {
  if (!people) return null
  const { spent, budget } = personSpend(people, personId)
  return budget ? ofLimit(spent, budget, true) : `${money(spent, true)} this month`
}

/** The owner's own row: their spend here; their limit is the agent's, set in Usage. */
export function OwnerSpendRow({ people }: { people: AgentPeople | null }) {
  return (
    <div className="settings-row">
      <Icon name="person.crop.circle" size={22} color="var(--secondary)" />
      <VStack grow className="settings-stack">
        <Text truncate>You</Text>
        <Text size="caption" tone="secondary">{[spendLine(people, 'owner'), 'Owner', 'your limit is in Usage'].filter(Boolean).join(' · ')}</Text>
      </VStack>
    </div>
  )
}

/** The thin bar under a person's line, in the agent's colours; none without a limit. */
export function PersonBar({ store, agent, people, personId }: { store: BotStore; agent: string; people: AgentPeople | null; personId: string }) {
  if (!people) return null
  const { spent, budget } = personSpend(people, personId)
  if (!budget) return null
  return <LimitBar value={fullness(spent, budget)} pair={pairOf(agentBot(store, agent)?.avatarColor)} label={ofLimit(spent, budget, true)} />
}

const CUSTOM = 'custom'
const NONE = 'none'

/** "Each month $20": a person's limit on this agent, the stock pop-up with a custom amount. */
export function BudgetPicker({ name, budget, busy, set }: { name: string; budget: number | null; busy: boolean; set: (b: number | null) => void }) {
  const preset = budget === null ? NONE : (BUDGET_PRESETS as readonly number[]).includes(budget) ? String(budget) : CUSTOM
  const [choice, setChoice] = useState(preset)
  const [custom, setCustom] = useState(budget && preset === CUSTOM ? String(budget) : '')
  useEffect(() => setChoice(preset), [preset])
  const amount = Number(custom)
  const okCustom = custom.trim() !== '' && Number.isFinite(amount) && amount > 0
  return (
    <div className="sp-budget">
      <select
        className="settings-select"
        value={choice}
        disabled={busy}
        aria-label={`Limit for ${name}, each month`}
        onChange={(e) => {
          const v = e.target.value
          setChoice(v)
          if (v === NONE) set(null)
          else if (v !== CUSTOM) set(Number(v))
        }}
      >
        {BUDGET_PRESETS.map((b) => <option key={b} value={String(b)}>{money(b)}</option>)}
        <option value={NONE}>No limit</option>
        <option value={CUSTOM}>Custom…</option>
      </select>
      {choice === CUSTOM ? (
        <form className="sp-budget-custom" onSubmit={(e) => { e.preventDefault(); if (okCustom) set(amount) }}>
          <span className="sp-custom-unit">$</span>
          <input inputMode="decimal" value={custom} onChange={(e) => setCustom(e.target.value)} aria-label={`Custom limit for ${name}`} placeholder="25" />
          <button type="submit" className="sp-budget-set" disabled={busy || !okCustom || amount === budget}>Set</button>
        </form>
      ) : null}
    </div>
  )
}

/** LIMIT FOR each person who holds this agent: what their runs may cost a month before they stop and you get a card. */
export function PeopleLimits({ holders, people, setBudget }: {
  holders: { personId: string; name: string }[]
  people: AgentPeople | null
  setBudget: (person: string, budget: number | null) => Promise<void>
}) {
  const [busy, setBusy] = useState<string | null>(null)
  if (!holders.length || !people) return null
  return (
    <VStack gap={6}>
      <Text size="caption" tone="secondary">Limits each month</Text>
      <CardSection footer="A person's runs stop at their limit and you get a card. It counts runs they start and helpers' calls for them, at API prices. New people start at $20 a month.">
        {holders.map((h) => (
          <div key={h.personId} className="settings-row">
            <VStack grow className="settings-stack">
              <Text truncate>{`Limit for ${h.name}`}</Text>
              <Text size="caption" tone="secondary">{spendLine(people, h.personId)}</Text>
            </VStack>
            <BudgetPicker
              name={h.name}
              budget={personSpend(people, h.personId).budget}
              busy={busy !== null}
              set={(b) => { setBusy(h.personId); void setBudget(h.personId, b).finally(() => setBusy(null)) }}
            />
          </div>
        ))}
      </CardSection>
    </VStack>
  )
}
