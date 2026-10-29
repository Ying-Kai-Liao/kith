import { useState, type ReactNode } from 'react'
import {
  ago, cronWords, formatUsd, lastRunText, packageNotes, rolePrompt, shortVersion, topModels,
  type KithInfo, type KithMcp, type KithSchedule, type KithSkill,
} from '@shared/kith-info'
import { Button, Disclosure, DropdownMenu, IconButton, Pill, Spinner } from '../../../components/Controls'
import { Icon } from '../../../components/Icon'
import { AgentBackups } from './AgentBackups'
import { MarkdownText } from '../../thread/Markdown'
import type { KithInfoStore } from '../../../store/kith-info-store'
import './kith.css'

/** The package chip, under the bot's name: "kith · house 1.4" in mono, opening what the package's state is (changes, unpushed, pinned, version). */
export function KithHeader({ agent, kith }: { agent: string; kith: KithInfoStore }) {
  if (kith.unsupported) return null
  const pkg = kith.info?.package
  const version = shortVersion(pkg?.version)
  const notes = pkg ? packageNotes(pkg) : []
  const label = `kith · ${agent}${pkg && version ? ` ${version}${pkg.dirty ? ' •' : ''}` : ''}`
  return (
    <DropdownMenu
      className="kith-chip-button"
      title={notes.join('\n') || undefined}
      items={() => [
        ...(version ? [{ title: `Version ${version}`, action: () => {} }] : []),
        ...(notes.length ? notes.map((note) => ({ title: note, action: () => {} })) : [{ title: 'No local changes', action: () => {} }]),
      ]}
    >
      <span>{label}</span>
      <Icon name="chevron.up.chevron.down" size={8} weight="semibold" />
    </DropdownMenu>
  )
}

/**
 * The Kith sections of a bot's details in the agent sheet's order: about (with the settings rows
 * the parent hands in), cost, schedules and routines (one list: `routines` renders the card and
 * gets kith's read-only schedules as its last rows), projects, harness, skills and tools.
 */
export function KithDetails({ kith, about, routines }: {
  kith: KithInfoStore
  about?: ReactNode
  routines?: (extra: ReactNode) => ReactNode
}) {
  if (kith.unsupported) return null
  const { info } = kith
  if (!info) {
    if (kith.error) return <Banner message={kith.error} loading={kith.loading} retry={() => void kith.load(true)} />
    if (kith.loaded) return <p className="kith-muted">No kith details for this bot</p>
    return <p className="kith-muted kith-loading" role="status"><Spinner size={12} /> Loading details…</p>
  }
  const unread = (key: string) => info.errors[key] !== undefined
  const schedules = unread('schedules')
    ? <Failed key="s" what="the schedules" />
    : info.schedules.map((s) => <ScheduleRow key={s.id} schedule={s} />)
  return (
    <>
      <About info={info} failed={unread('role')}>{about}</About>
      <Cost info={info} failed={unread('cost')} />
      {routines ? routines(schedules) : null}
      {info.errors.projects !== undefined || info.projects.length ? <Projects info={info} failed={unread('projects')} /> : null}
      <ModelAndPermissions info={info} />
      <AgentBackups agent={info.agent} />
      <Lists info={info} />
      <div className="sheet-footer kith-refresh">
        {kith.refreshError ? <span className="kith-failed" role="alert">Refresh failed: {kith.refreshError}</span>
          : <span>{kith.fetchedAt ? `Updated ${new Date(kith.fetchedAt).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}` : ''}</span>}
        {kith.loading ? <Spinner size={12} /> : <IconButton title="Refresh details" icon="arrow.clockwise" onClick={() => void kith.load(true)} />}
      </div>
    </>
  )
}

function Banner({ message, loading, retry }: { message: string; loading: boolean; retry: () => void }) {
  return (
    <div className="kith-banner" role="alert">
      <span>Kith details unavailable: {message}</span>
      <Button kind="secondary" disabled={loading} onClick={retry}>{loading ? 'Retrying…' : 'Retry'}</Button>
    </div>
  )
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="details-section">
      <span className="details-section-title">{title}</span>
      {children}
    </section>
  )
}

const Failed = ({ what }: { what: string }) => <p className="kith-muted kith-failed">Couldn't read {what}</p>

/** The description stays in view; the long role prompt (the agent's instructions) is one row, closed until opened. */
function About({ info, failed, children }: { info: KithInfo; failed: boolean; children?: ReactNode }) {
  const [open, setOpen] = useState(false)
  const prompt = rolePrompt(info.role)
  return (
    <Section title="About">
      <div className="outlined-card kith-card">
        {failed ? <Failed what="the role" /> : <>
          {info.description ? <p className="kith-description">{info.description}</p> : null}
          {prompt ? (
            <Disclosure expanded={open} onToggle={() => setOpen(!open)} label={<span className="kith-row-title">Instructions</span>}>
              <MarkdownText text={prompt} />
            </Disclosure>
          ) : null}
        </>}
        {children}
      </div>
    </Section>
  )
}

type Range = 'today' | 'week' | 'month' | 'all'

function Cost({ info, failed }: { info: KithInfo; failed: boolean }) {
  const cost = info.cost
  const [range, setRange] = useState<Range>('month')
  const ranges: [Range, string][] = [['today', 'Today'], ['week', '7 d'], ['month', '30 d'], ['all', 'All']]
  return (
    <Section title="Cost">
      <div className="outlined-card kith-card">
        {failed ? <Failed what="the cost" /> : !cost ? <p className="kith-muted">No cost recorded yet</p> : <>
          <div className="sheet-range" role="tablist" aria-label="Cost range">
            {ranges.map(([id, label]) => (
              <button key={id} role="tab" aria-selected={range === id} className={range === id ? 'on' : ''} onClick={() => setRange(id)}>{label}</button>
            ))}
          </div>
          <div className="sheet-amount-row">
            <span className="sheet-amount">{formatUsd(cost[range])}</span>
            {/* The host sends requests and the per-model split for 30 days only. */}
            {range === 'month' ? <span className="kith-muted">{cost.requestsMonth} {cost.requestsMonth === 1 ? 'request' : 'requests'}</span> : null}
          </div>
          {range === 'month' ? topModels(cost).map((m) => (
            <div key={m.model} className="sheet-bar" title={`${m.model} · ${Math.round(m.share * 100)}%`}>
              <span className="kith-ellipsis">{m.model}</span>
              <i style={{ width: `${Math.max(2, Math.round(m.share * 100))}%` }} />
              <em>{formatUsd(m.usd)}</em>
            </div>
          )) : null}
        </>}
      </div>
    </Section>
  )
}

function ScheduleRow({ schedule: s }: { schedule: KithSchedule }) {
  const [open, setOpen] = useState(false)
  const last = lastRunText(s)
  return (
    <div className="kith-schedule">
      <button className={`kith-task press${open ? ' open' : ''}`} onClick={() => setOpen(!open)} aria-expanded={open} title={open ? undefined : s.task}>
        {s.task || s.id}
      </button>
      <div className="kith-meta">
        <span>{cronWords(s.cron)}</span>
        {s.paused ? <Pill outlined className="kith-badge">Paused</Pill> : null}
        {last ? <span>{last === 'running' ? 'running' : `last ${last}`}</span> : null}
        {s.nextRun !== null && !s.paused ? <span>next {new Date(s.nextRun).toLocaleString([], { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })}</span> : null}
      </div>
    </div>
  )
}

function Projects({ info, failed }: { info: KithInfo; failed: boolean }) {
  return (
    <Section title="Projects">
      <div className="outlined-card kith-card">
        {failed ? <Failed what="the projects" /> : info.projects.map((p) => (
          <div key={p.id} className="kith-model">
            <span className="kith-ellipsis" title={p.id}>{p.id}</span>
            <span className="kith-muted">{p.files} {p.files === 1 ? 'file' : 'files'}{p.modified !== null ? ` · ${ago(p.modified)}` : ''}</span>
          </div>
        ))}
      </div>
    </Section>
  )
}

/** Model, effort and permission are in the bot's settings (hq-backed rows, once); what is left here is the harness. */
function ModelAndPermissions({ info }: { info: KithInfo }) {
  const rows: [string, string | null][] = [
    ['Harness', info.harness],
    ['Read-only', info.readOnly ? 'yes' : null],
  ]
  const shown = rows.filter(([, v]) => v)
  if (!shown.length) return null
  return (
    <Section title="Harness">
      <div className="outlined-card kith-card">
        {shown.map(([label, value]) => (
          <div key={label} className="kith-model"><span>{label}</span><span className="kith-muted">{value}</span></div>
        ))}
        <small className="kith-muted">Set in the agent's package</small>
      </div>
    </Section>
  )
}

/** Skills, tools, MCP servers and plugins: a collapsed group each, with its count. */
function Lists({ info }: { info: KithInfo }) {
  const group = (key: string, title: string, count: number, body: ReactNode) => (
    info.errors[key] !== undefined || count > 0 ? <Group key={key} title={title} count={info.errors[key] !== undefined ? null : count}>{info.errors[key] !== undefined ? <Failed what={title.toLowerCase()} /> : body}</Group> : null
  )
  return (
    <Section title="Skills, tools, plugins">
      <div className="outlined-card kith-card">
        {group('skills', 'Skills', info.skills.length, info.skills.map((s) => <SkillRow key={s.name} skill={s} />))}
        {group('tools', 'Tools', info.tools.length, info.tools.map((t) => <div key={t} className="kith-item kith-ellipsis" title={t}>{t}</div>))}
        {group('mcp_servers', 'MCP servers', info.mcpServers.length, info.mcpServers.map((m) => <McpRow key={m.name} server={m} />))}
        {group('plugins', 'Plugins', info.plugins.length, info.plugins.map((t) => <div key={t} className="kith-item kith-ellipsis" title={t}>{t}</div>))}
        {!info.skills.length && !info.tools.length && !info.mcpServers.length && !info.plugins.length && !Object.keys(info.errors).length
          ? <p className="kith-muted">Nothing set up</p> : null}
      </div>
    </Section>
  )
}

function Group({ title, count, children }: { title: string; count: number | null; children: ReactNode }) {
  const [open, setOpen] = useState(false)
  return (
    <Disclosure expanded={open} onToggle={() => setOpen(!open)} label={<span className="kith-row-title">{title}{count !== null ? <span className="kith-muted"> · {count}</span> : null}</span>}>
      <div className="kith-group">{children}</div>
    </Disclosure>
  )
}

function SkillRow({ skill }: { skill: KithSkill }) {
  return (
    <div className="kith-item">
      <div className="kith-skill-head">
        <strong className="kith-ellipsis">{skill.name}</strong>
        {skill.source === 'learned' || skill.source === 'global' ? <Pill outlined className="kith-badge">{skill.source}</Pill> : null}
      </div>
      {skill.description ? <p className="kith-muted kith-skill-desc" title={skill.description}>{skill.description}</p> : null}
    </div>
  )
}

function McpRow({ server }: { server: KithMcp }) {
  const where = server.command ?? server.url ?? ''
  return (
    <div className="kith-item">
      <div className="kith-skill-head"><strong className="kith-ellipsis">{server.name}</strong>{server.kind ? <Pill outlined className="kith-badge">{server.kind}</Pill> : null}</div>
      {where ? <p className="kith-muted kith-ellipsis" title={where}>{where}</p> : null}
    </div>
  )
}
