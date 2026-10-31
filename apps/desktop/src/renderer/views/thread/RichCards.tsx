import { useState } from 'react'
import type { Entry } from '@shared/models'
import type { RichCard } from '@shared/rich-card'
import { ownerNameFrom } from '@shared/mac-role'
import { scheduleFoot, scheduleView, type ScheduleFold, type ScheduleOp, type ScheduleView } from '@shared/schedule-card'
import { newsView, type NewsRow } from '@shared/news-card'
import { resultView } from '@shared/result-card'
import { planView } from '@shared/plan-card'
import { BotAvatar } from '../../components/Avatar'
import { pairOf } from '../../components/kith'
import { pairStyle } from '../../components/kith/pair'
import { Icon } from '../../components/Icon'
import { useStore } from '../../store/context'
import { useLiveSchedule } from './rich-card-live'
import './rich-card.css'

const personName = (id: string) => id.charAt(0).toUpperCase() + id.slice(1)

/** A card folded to one line: a changed or removed job, or a card after its day (tap opens it in place). */
function FoldedCard({ fold, open }: { fold: ScheduleFold; open: () => void }) {
  const body = (
    <>
      {fold.done ? <Icon name="checkmark" size={10} color="var(--kith-positive)" /> : null}
      <Icon name="clock" size={11} color="var(--kith-muted)" />
      <span className="rc-pill-text">{fold.text}</span>
      <span className="rc-pill-time">{fold.trailing}</span>
    </>
  )
  return fold.opens ? <button className="rc-pill" onClick={open}>{body}</button> : <div className="rc-pill">{body}</div>
}

/** The owner's buttons (Pause or Resume, Run now, trash with its confirm in the card), or who can press them. */
function ScheduleButtons({ view, act, ownerName }: { view: ScheduleView; act: ((op: ScheduleOp) => Promise<void>) | null; ownerName: string | null }) {
  const [confirming, setConfirming] = useState(false)
  const [busy, setBusy] = useState<ScheduleOp | null>(null)
  const [error, setError] = useState<string | null>(null)
  const foot = scheduleFoot(view, { owner: act !== null, ownerName, confirming, busy })
  const press = async (op: ScheduleOp) => {
    if (!act) return
    setBusy(op)
    setError(null)
    try {
      await act(op)
      setConfirming(false)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
    setBusy(null)
  }
  if (foot.kind === 'note') return <div className="rc-note">{foot.text}</div>
  return (
    <>
      {foot.kind === 'confirm' ? (
        <div className="rc-confirm">
          <span className="rc-confirm-text">{foot.question}</span>
          <div className="rc-buttons">
            <button className="rc-button danger" disabled={foot.removing} onClick={() => void press('remove')}>Remove</button>
            <button className="rc-button" disabled={foot.removing} onClick={() => { setConfirming(false); setError(null) }}>Cancel</button>
          </div>
        </div>
      ) : (
        <div className="rc-buttons">
          <button className={foot.toggle.ink ? 'rc-button ink' : 'rc-button'} disabled={foot.busy} onClick={() => void press(foot.toggle.op)}>{foot.toggle.label}</button>
          <button className="rc-button" disabled={foot.run.disabled} onClick={() => void press('run')}>{foot.run.label}</button>
          <button className="rc-trash" disabled={foot.busy} aria-label={`Remove ${view.name}`} title="Remove" onClick={() => { setConfirming(true); setError(null) }}>
            <Icon name="trash" size={13} color="var(--kith-muted)" />
          </button>
        </div>
      )}
      {error ? <div className="rc-action-error" role="alert">{error}</div> : null}
    </>
  )
}

/** The Schedule card: a Kith agent's kith-sched job or an outside bot's routine (DESIGN.md "Schedule"). */
export function ScheduleCard({ entry, card }: { entry: Entry; card: RichCard }) {
  const store = useStore()
  const [opened, setOpened] = useState(false)
  const [allTask, setAllTask] = useState(false)
  const { live, act } = useLiveSchedule(card, entry)
  const person = typeof card.snapshot?.person === 'string' ? card.snapshot.person : null
  const view = scheduleView(card, {
    postedAt: entry.createdAt,
    now: Date.now(),
    live,
    // The card posts in the 1:1 of the person the job runs as: theirs when it names them, else the owner's.
    viewerIsPerson: person ? !store.isOwner : store.isOwner,
    personName: person ? personName(person) : null,
    opened,
  })
  if (view.fold) return <FoldedCard fold={view.fold} open={() => setOpened(true)} />

  const ref = card.ref ?? {}
  const runner = view.kind === 'routine' ? store.bots.get(String(ref.bot)) : ([...store.bots.values()].find((b) => b.hqAgent === ref.agent) ?? store.bots.get(entry.botId))
  return (
    <section className="rc-card" aria-label={`Schedule: ${view.name}`}>
      <div className="rc-head">
        <Icon name="clock" size={12} color="var(--kith-ink)" />
        <span className="rc-name">{view.name}</span>
        {view.paused ? <span className="rc-capsule">Paused</span> : <span className="rc-trailing">{view.op}</span>}
      </div>
      <div className={view.paused ? 'rc-every muted' : 'rc-every'}>{view.every}</div>
      <div className="rc-group">
        <div className="rc-row">
          <span className="rc-key">Next</span>
          <span className="rc-value">{view.next}</span>
        </div>
        <div className="rc-row">
          <span className="rc-key">Last</span>
          <span className="rc-value">
            {view.last.running ? <span className="rc-dots" aria-hidden><i /><i /><i /></span> : null}
            {view.last.text}
            {view.last.failed ? <span className="rc-failed"> · Failed</span> : null}
            {view.last.error ? <span className="rc-error">{view.last.error}</span> : null}
          </span>
        </div>
        <div className="rc-row">
          <span className="rc-key">Runs</span>
          <span className="rc-value rc-runs">
            {runner ? <BotAvatar bot={runner} size={16} animated={false} /> : null}
            <span>{runner?.name ?? String(ref.agent ?? 'The bot')}</span>
            {view.runsAs ? <><span className="rc-as">as</span><span>{view.runsAs}</span></> : null}
          </span>
        </div>
      </div>
      {view.task ? <button className={allTask ? 'rc-task all' : 'rc-task'} onClick={() => setAllTask(!allTask)}>{view.task}</button> : null}
      <ScheduleButtons view={view} act={store.isOwner ? act : null} ownerName={ownerNameFrom(store.bots.get(entry.botId)?.activity)} />
    </section>
  )
}

// Links opened this session go quiet, so a second look shows what's left.
const openedLinks = new Set<string>()

function NewsItem({ row }: { row: NewsRow }) {
  const [quiet, setQuiet] = useState(openedLinks.has(row.url))
  const open = () => {
    openedLinks.add(row.url)
    setQuiet(true)
    window.kith.app.openExternal(row.url)
  }
  return (
    <button className={quiet ? 'rc-news-row quiet' : 'rc-news-row'} title={row.url} onClick={open}>
      <span className="rc-news-title">{row.title}</span>
      {row.line ? <span className="rc-news-line">{row.line}</span> : null}
      <span className="rc-news-source">
        {row.source ? `${row.source} · ` : null}
        <span className="rc-news-domain">{row.domain}</span>
      </span>
    </button>
  )
}

/** The News card: a scheduled run's brief as rows that open their links (DESIGN.md "News"). */
export function NewsCard({ entry, card }: { entry: Entry; card: RichCard }) {
  const [expanded, setExpanded] = useState(false)
  const [opened, setOpened] = useState(false)
  const view = newsView(card, { postedAt: entry.createdAt, now: Date.now(), expanded, opened })
  if (view.fold) {
    const at = new Date(entry.createdAt)
    const trailing = `${at.getHours()}:${String(at.getMinutes()).padStart(2, '0')}`
    return <FoldedCard fold={{ text: view.fold, trailing, opens: true, done: false }} open={() => setOpened(true)} />
  }
  return (
    <section className="rc-card" aria-label={view.title}>
      <div className="rc-head">
        <Icon name="clock" size={12} color="var(--kith-ink)" />
        <span className="rc-name">{view.title}</span>
        {view.every ? <span className="rc-trailing">{view.every}</span> : null}
      </div>
      <div className="rc-group">
        {view.shown.map((row, i) => <NewsItem key={`${i}-${row.url}`} row={row} />)}
        {view.more > 0 ? (
          <button className="rc-news-more" onClick={() => setExpanded(true)}>
            {`Show ${view.more} more`}
            <Icon name="chevron.down" size={9} color="var(--kith-muted)" />
          </button>
        ) : null}
      </div>
    </section>
  )
}

/**
 * The Result card: a reply Jev sorted as a result. v2 is the Info card (DESIGN.md "Reply cards"): the reply's heading,
 * one sub line, Key: value rows and a link row if the reply had a link. v1 keeps its mono facts and buttons for old
 * entries. The reply stays; "Show as text" folds the card back to it in place.
 */
export function ResultCard({ card, showText }: { card: RichCard; showText: () => void }) {
  const view = resultView(card)
  if (!view) return null
  if (view.rows.length) {
    return (
      <section className="rc-card info" aria-label={view.title}>
        <div className="rc-head"><span className="rc-name wrap">{view.title}</span></div>
        {view.sub ? <div className="rc-sub">{view.sub}</div> : null}
        <div className="rc-info">
          {view.rows.map((r, i) => (
            <div key={i} className="rc-info-row">
              {r.label ? <span className="rc-info-label">{r.label}</span> : null}
              <span className={r.label ? 'rc-info-value' : 'rc-info-value alone'}>{r.value}</span>
            </div>
          ))}
          {view.links.map((l) => (
            <button key={l.url} className="rc-info-row rc-info-link" title={l.url} onClick={() => window.kith.app.openExternal(l.url)}>
              <span className="rc-info-value">{l.label}</span>
              <Icon name="arrow.up.right" size={11} color="var(--rc-faint)" />
            </button>
          ))}
        </div>
        <button className="rc-as-text-link" onClick={showText}>
          <Icon name="text.alignleft" size={11} color="var(--kith-muted)" />
          Show as text
        </button>
      </section>
    )
  }
  return (
    <section className="rc-card" aria-label={view.title}>
      <div className="rc-head">
        <span className="rc-tile" aria-hidden><Icon name="doc.text" size={13} color="var(--kith-ink)" /></span>
        <span className="rc-name">{view.title}</span>
      </div>
      <div className="rc-facts">{view.facts.join(' · ')}</div>
      <div className="rc-buttons">
        {view.links.map((l, i) => (
          <button key={l.url} className={i === 0 ? 'rc-button ink' : 'rc-button'} title={l.url} onClick={() => window.kith.app.openExternal(l.url)}>{l.label}</button>
        ))}
        <button className="rc-button rc-as-text" onClick={showText}>Show as text</button>
      </div>
    </section>
  )
}

/** The Plan card: steps updating in place under a count and a bar in the agent's colour pair (DESIGN.md "Plan"). */
export function PlanCard({ entry, card }: { entry: Entry; card: RichCard }) {
  const store = useStore()
  const view = planView(card)
  const pair = pairOf(store.bots.get(entry.data.author ?? entry.botId)?.avatarColor)
  return (
    <section className="rc-card" aria-label={`${view.title}, ${view.count}`}>
      <div className="rc-head">
        <Icon name="checklist" size={12} color="var(--kith-ink)" />
        <span className="rc-name">{view.title}</span>
        <span className="rc-trailing rc-mono">{view.count}</span>
      </div>
      <div className="rc-bar" style={pairStyle(pair)} role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(view.fraction * 100)}>
        <i style={{ width: `${view.fraction * 100}%` }} />
      </div>
      <ol className="rc-plan">
        {view.steps.map((step, i) => (
          <li key={i} className={`rc-step ${step.state}`}>
            <span className="rc-mark" aria-hidden>
              {step.state === 'done' ? <Icon name="checkmark" size={10} color="var(--kith-muted)" /> : <i />}
            </span>
            <span className="rc-step-text">{step.text}</span>
            <span className="rc-sr">{step.state === 'stopped' ? 'stopped' : step.state}</span>
          </li>
        ))}
      </ol>
    </section>
  )
}
