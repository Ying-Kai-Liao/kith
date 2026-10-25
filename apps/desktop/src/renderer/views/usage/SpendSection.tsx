import { useEffect, useState } from 'react'
import { isUnsupportedHost } from '@shared/kith-info'
import { isGroup, type Bot } from '@shared/models'
import {
  AT_LIMIT, atLimitTitle, fullness, LIMIT_PRESETS, limitForecast, modelLine, money, nextMonthStart, ofLimit, periodChoices,
  runsLine, runTitle, shortDay, SHOWN_AGENTS, type AgentUsage, type AtLimit, type Period, type UsageReport, type UsageRow,
} from '@shared/usage-limits'
import { BotAvatar } from '../../components/Avatar'
import { CardSection, SegmentedChoice, Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { pairOf, type Pair } from '../../components/kith/pair.ts'
import { ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import type { BotStore } from '../../store/bot-store'
import './spend.css'

const message = (e: unknown) => (e instanceof Error ? e.message : String(e))

/** The owner's own chat with an agent: its face and colours on these screens. */
export function agentBot(store: BotStore, agent: string): Bot | undefined {
  return [...store.bots.values()].find((b) => b.hqAgent === agent && !isGroup(b))
}

/** A limit's thin bar in the agent's two colours. */
export function LimitBar({ value, pair, label }: { value: number; pair: Pair; label: string }) {
  return (
    <div className="sp-limit-bar" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(value * 100)}>
      <i style={{ width: `${Math.max(2, value * 100)}%`, background: `linear-gradient(90deg, ${pair.a}, ${pair.b})` }} />
    </div>
  )
}

/**
 * Usage's cost part, under the plan meter: the period's total at API prices split into agents and your own sessions,
 * a row per agent (a thin bar in its colours when it has a limit), and your own sessions last. An agent opens its sheet.
 */
export function SpendSection({ store, title }: { store: BotStore; title: string | null }) {
  useModel(store)
  const [period, setPeriod] = useState<Period>('month')
  const [report, setReport] = useState<UsageReport | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [unsupported, setUnsupported] = useState(false)
  const [all, setAll] = useState(false)
  const [open, setOpen] = useState<string | null>(null)
  const [stamp, setStamp] = useState(0)
  const online = store.connection.kind === 'online'
  useEffect(() => {
    if (!online) return
    let live = true
    void (async () => {
      try {
        const r = await (await store.ready()).usageReport(period)
        if (live) { setReport(r); setError(null) }
      } catch (e) {
        if (!live) return
        if (isUnsupportedHost(e)) setUnsupported(true)
        else setError(message(e))
      }
    })()
    return () => { live = false }
  }, [store, period, online, stamp])
  if (unsupported || !store.isOwner) return null
  const rows = report?.rows ?? []
  const shown = all ? rows : rows.slice(0, SHOWN_AGENTS)
  const more = rows.length - shown.length
  const agentsShare = report && report.total > 0 ? report.agents / report.total : 0
  return (
    <section className="sp" aria-label={title ? `${title} · cost` : 'Cost'}>
      <SegmentedChoice<Period> selection={period} options={periodChoices()} onChange={setPeriod} />
      {report ? (
        <>
          <div className="sp-total">
            <Text size={28} weight="semibold" tabular className="sp-total-amount">{money(report.total)}</Text>
            <Text size="callout" tone="secondary">{`${report.label} at API prices`}</Text>
          </div>
          <div className="sp-split" aria-hidden>
            <i style={{ width: `${agentsShare * 100}%` }} />
          </div>
          <div className="sp-legend">
            <span><i className="sp-dot ink" />Agents <b>{money(report.agents)}</b></span>
            <span><i className="sp-dot" />Your own sessions <b>{money(report.own)}</b></span>
          </div>
          <CardSection title="Agents" accessory={more > 0 ? <button className="sp-more" onClick={() => setAll(true)}>{`${more} more`}</button> : null}>
            {shown.map((r) => <AgentRow key={r.agent} store={store} row={r} onOpen={() => setOpen(r.agent)} />)}
            <div className="sp-row own">
              <span className="sp-own-icon"><Icon name="terminal" size={14} /></span>
              <span className="sp-row-text">
                <Text tone="secondary">Your own sessions</Text>
                <Text size="caption" tone="tertiary">Terminal and flow workers · no agent</Text>
              </span>
              <Text tone="secondary" tabular>{money(report.own)}</Text>
            </div>
          </CardSection>
        </>
      ) : error ? (
        <Text size="footnote" tone="danger">{error}</Text>
      ) : (
        <div className="sp-loading"><Spinner size={14} /></div>
      )}
      <Sheet className="settings-sheet" open={open !== null} onClose={() => setOpen(null)} width={460}>
        {open ? <AgentSpendSheet store={store} agent={open} changed={() => setStamp((s) => s + 1)} /> : null}
      </Sheet>
    </section>
  )
}

function AgentRow({ store, row, onOpen }: { store: BotStore; row: UsageRow; onOpen: () => void }) {
  const bot = agentBot(store, row.agent)
  const pair = pairOf(bot?.avatarColor)
  const spent = row.month ?? row.cost
  return (
    <button className="sp-row" onClick={onOpen} aria-label={`${row.agent}, ${money(row.cost)}`}>
      {bot ? <BotAvatar bot={bot} size={30} animated={false} /> : <span className="sp-face-blank"><Icon name="person.crop.circle" size={18} /></span>}
      <span className="sp-row-text">
        <Text weight="semibold" truncate>{row.agent}</Text>
        <Text size="caption" tone="secondary">{runsLine(row.runs, row.perRun)}</Text>
        {row.limit ? <LimitBar value={fullness(spent, row.limit)} pair={pair} label={`${ofLimit(spent, row.limit)} this month`} /> : null}
      </span>
      <span className="sp-row-value">
        <Text weight="semibold" tabular>{money(row.limit ? spent : row.cost)}</Text>
        {row.limit ? <Text size="caption" tone="secondary" tabular>{` of ${money(row.limit)}`}</Text> : null}
      </span>
      <Icon name="chevron.right" size={10} color="var(--tertiary)" />
    </button>
  )
}

/** One agent: this month's cost, days, limit and forecast, what sets the cost, its costliest runs; the limit sheet on top. */
function AgentSpendSheet({ store, agent, changed }: { store: BotStore; agent: string; changed: () => void }) {
  const [view, setView] = useState<AgentUsage | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [editing, setEditing] = useState(false)
  useEffect(() => {
    let live = true
    void (async () => {
      try {
        const v = await (await store.ready()).agentUsage(agent, 'month')
        if (live) setView(v)
      } catch (e) {
        if (live) setError(message(e))
      }
    })()
    return () => { live = false }
  }, [store, agent])
  const bot = agentBot(store, agent)
  const pair = pairOf(bot?.avatarColor)
  const title = (
    <span className="sp-sheet-title">
      {bot ? <BotAvatar bot={bot} size={22} animated={false} /> : null}
      {agent}
    </span>
  )
  if (editing && view) {
    return (
      <LimitPage store={store} view={view} bot={bot} cancel={() => setEditing(false)} saved={(v) => { setView(v); setEditing(false); changed() }} />
    )
  }
  return (
    <SheetPage className="sp-sheet">
      <ModalHeader title={title} />
      {view ? (
        <div className="sp-sheet-body">
          <div>
            <Text size={30} weight="semibold" tabular>{money(view.month, true)}</Text>
            <div><Text size="callout" tone="secondary">{`${view.monthLabel} · ${runsLine(view.runs, view.perRun)} · at API prices`}</Text></div>
          </div>
          {view.limit ? (
            <div className="sp-limit-line">
              <div className="sp-line"><Text>Limit</Text><Text tabular>{ofLimit(view.month, view.limit)}</Text></div>
              <LimitBar value={fullness(view.month, view.limit)} pair={pair} label={ofLimit(view.month, view.limit)} />
            </div>
          ) : null}
          {view.forecast ? <Text size="caption" tone="secondary">{forecastLine(view)}</Text> : null}
          <DayBars days={view.days} />
          <CardSection title="Limit">
            <button className="sp-row" onClick={() => setEditing(true)}>
              <span className="sp-row-text"><Text>Monthly limit</Text></span>
              <Text tone="secondary" tabular>{view.limit ? money(view.limit) : 'No limit'}</Text>
              <Icon name="chevron.right" size={10} color="var(--tertiary)" />
            </button>
            <button className="sp-row" onClick={() => setEditing(true)}>
              <span className="sp-row-text"><Text>At the limit</Text></span>
              <Text tone="secondary">{atLimitTitle(view.at)}</Text>
              <Icon name="chevron.right" size={10} color="var(--tertiary)" />
            </button>
            <div className="sp-row">
              <span className="sp-row-text">
                <Text>Model and effort</Text>
                <Text size="caption" tone="secondary">Most of the cost is chosen here</Text>
              </span>
              <Text tone="secondary">{modelLine(view.models)}</Text>
            </div>
          </CardSection>
          {view.costliest.length ? (
            <CardSection title="Costliest runs">
              {view.costliest.map((r) => (
                <div key={r.session} className="sp-row">
                  <span className="sp-row-text">
                    <Text truncate>{runTitle(r.turn)}</Text>
                    <Text size="caption" tone="secondary">{`${shortDay(r.started)} · ${r.via || 'app'}`}</Text>
                  </span>
                  <Text tabular>{money(r.cost, true)}</Text>
                </div>
              ))}
            </CardSection>
          ) : null}
        </div>
      ) : error ? (
        <div className="sp-sheet-body"><Text size="footnote" tone="danger">{error}</Text></div>
      ) : (
        <div className="sp-loading"><Spinner size={14} /></div>
      )}
    </SheetPage>
  )
}

/** "About $72 a day since Oct 23, so it reaches $300 today." with a limit; the ledger's own words without. */
function forecastLine(v: AgentUsage): string {
  const f = v.forecast!
  if (!v.limit) return f.text
  const pace = `About ${money(f.perDay)} a day since ${shortDay(f.since)}`
  if (v.month >= v.limit) return `${pace}; it has used its ${money(v.limit)}.`
  const day = f.reach[String(v.limit)]
  const today = new Date()
  const iso = `${today.getFullYear()}-${String(today.getMonth() + 1).padStart(2, '0')}-${String(today.getDate()).padStart(2, '0')}`
  if (!day) return `${pace}, so ${money(v.limit)} lasts the month.`
  return `${pace}, so it reaches ${money(v.limit)} ${day <= iso ? 'today' : `about ${shortDay(day)}`}.`
}

/** This month's days as ink bars, the first and today labelled. */
function DayBars({ days }: { days: AgentUsage['days'] }) {
  const top = Math.max(0.01, ...days.map((d) => d.cost))
  if (!days.length) return null
  return (
    <div className="sp-days">
      <div className="sp-days-bars" role="img" aria-label="Cost per day this month">
        {days.map((d) => <i key={d.day} style={{ height: `${Math.max(d.cost > 0 ? 4 : 1, (d.cost / top) * 100)}%` }} title={`${shortDay(d.day)} · ${money(d.cost, true)}`} />)}
      </div>
      <div className="sp-line"><Text size="caption" tone="tertiary">{shortDay(days[0]!.day)}</Text><Text size="caption" tone="tertiary">Today</Text></div>
    </div>
  )
}

/** The limit sheet: presets or a custom amount, the forecast in words, and what happens at the limit. */
function LimitPage({ store, view, bot, cancel, saved }: { store: BotStore; view: AgentUsage; bot: Bot | undefined; cancel: () => void; saved: (v: AgentUsage) => void }) {
  const known = view.limit === null || (LIMIT_PRESETS as readonly number[]).includes(view.limit)
  const [limit, setLimit] = useState<number | null>(view.limit)
  const [custom, setCustom] = useState(known ? '' : String(view.limit))
  const [customOn, setCustomOn] = useState(!known)
  const [at, setAt] = useState<AtLimit>(view.at)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const typed = Number(custom)
  const chosen = customOn ? (custom.trim() && typed > 0 ? typed : null) : limit
  const invalid = customOn && !(typed > 0)
  const done = async () => {
    if (busy || invalid) return
    setBusy(true)
    setError(null)
    try {
      saved(await (await store.ready()).setAgentLimit(view.agent, chosen, at))
    } catch (e) {
      setError(message(e))
      setBusy(false)
    }
  }
  const preset = (value: number | null, label: string) => (
    <button key={label} className={`sp-preset ${!customOn && limit === value ? 'on' : ''}`} aria-pressed={!customOn && limit === value} onClick={() => { setCustomOn(false); setLimit(value) }}>{label}</button>
  )
  return (
    <SheetPage className="sp-sheet">
      <div className="sp-limit-top">
        <button className="sp-close" onClick={cancel} aria-label="Close"><Icon name="xmark" size={12} /></button>
        <button className="sp-done" onClick={() => void done()} disabled={busy || invalid}>{busy ? <Spinner size={12} /> : 'Done'}</button>
      </div>
      <div className="sp-sheet-body">
        <div className="sp-limit-head">
          {bot ? <BotAvatar bot={bot} size={36} animated={false} /> : null}
          <div>
            <Text size="title3" weight="semibold">Monthly limit</Text>
            <div><Text size="caption" tone="secondary">{`${view.agent} · starts again ${nextMonthStart()} · at API prices`}</Text></div>
          </div>
        </div>
        <div className="sp-presets" role="group" aria-label="Monthly limit">
          {preset(null, 'No limit')}
          {LIMIT_PRESETS.map((p) => preset(p, money(p)))}
          <button className={`sp-preset ${customOn ? 'on' : ''}`} aria-pressed={customOn} onClick={() => setCustomOn(true)}>Custom</button>
        </div>
        {customOn ? (
          <label className="sp-custom">
            <span>$</span>
            <input autoFocus inputMode="decimal" value={custom} onChange={(e) => setCustom(e.target.value.replace(/[^0-9.]/g, ''))} aria-label="Dollars a month" placeholder="250" />
            <span className="sp-custom-unit">a month</span>
          </label>
        ) : null}
        <Text size="caption" tone="secondary">{limitForecast(view.forecast, view.month, chosen)}</Text>
        <CardSection title="At the limit">
          {AT_LIMIT.map((a) => (
            <button key={a.id} className="sp-row sp-choice" aria-pressed={at === a.id} onClick={() => setAt(a.id)}>
              <span className="sp-check">{at === a.id ? <Icon name="checkmark" size={12} weight="semibold" /> : null}</span>
              <span className="sp-row-text">
                <Text>{a.title}</Text>
                <Text size="caption" tone="secondary">{a.detail}</Text>
              </span>
            </button>
          ))}
        </CardSection>
        <Text size="footnote" tone="tertiary">Your own runs never just stop at the limit: Ask me gives you a card, and Hold is only there if you pick it.</Text>
        {error ? <Text size="footnote" tone="danger">{error}</Text> : null}
      </div>
    </SheetPage>
  )
}
