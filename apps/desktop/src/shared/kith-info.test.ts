import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import {
  accessGrantHolder, accessGrantText, cronWords, formatUsd, isUnsupportedHost, lastRunText, packageNotes, parseAccessGrants, parseKithInfo, parseKithInfoResult,
  parseSearchResult, rolePrompt, shortVersion, splitRoleSections, topModels,
} from './kith-info.ts'

// `kith house --info-json` as printed on a real Mac (role text and memory files trimmed).
const sample = JSON.parse(readFileSync(new URL('./test-support/kith-house-info.json', import.meta.url), 'utf8'))

test('a real kith house --info-json parses', () => {
  const info = parseKithInfo(sample)!
  assert.equal(info.agent, 'house')
  assert.equal(info.model, 'sonnet')
  assert.equal(info.permissionMode, 'auto')
  assert.equal(info.skills.length, 4)
  assert.equal(info.skills[0].source, 'package')
  assert.equal(info.tools.length, 3)
  assert.deepEqual(info.mcpServers, [])
  assert.equal(info.schedules.length, 0)
  assert.equal(shortVersion(info.package?.version), '393def5')
  assert.equal(info.cost?.requestsMonth, 146)
  assert.equal(info.memory?.files.length, 3)
  assert.equal(info.memory?.totalBytes, 3920)
  // memory files carry ISO dates
  assert.equal(info.memory?.files[0].modified, Date.parse('2026-10-14T00:56:37+11:00'))
  assert.deepEqual(info.errors, {})
})

test('the boundary section stands apart from the others', () => {
  const info = parseKithInfo(sample)!
  const { notYours, rest } = splitRoleSections(info.role!.sections)
  assert.equal(notYours.length, 1)
  assert.equal(notYours[0].title, 'Not yours')
  assert.equal(rest.length, 2)
  assert.equal(splitRoleSections([{ title: ' NOT YOURS ', text: '' }]).notYours.length, 1)
})

test('missing and odd fields still parse', () => {
  const info = parseKithInfo({ agent: 'x', skills: [{ name: 'a' }, { description: 'no name' }, 3], cost: null, memory: { files: [{ bytes: 1 }] }, errors: { cost: 'boom' } })!
  assert.deepEqual(info.skills, [{ name: 'a', description: '', source: 'package' }])
  assert.equal(info.cost, null)
  assert.deepEqual(info.memory, { files: [], totalBytes: 0 })
  assert.equal(info.errors.cost, 'boom')
  assert.equal(parseKithInfo('nope'), null)
  assert.deepEqual(parseKithInfoResult({ info: null, error: 'kith is missing' }), { info: null, error: 'kith is missing', fetchedAt: null })
})

test('schedule times accept unix seconds, milliseconds and ISO dates', () => {
  const info = parseKithInfo({ schedules: [{ id: 'a', cron: '0 9 * * *', task: 't', paused: true, last_run: 1760000000, last_status: 'done', next_run: '2026-10-23T09:00:00Z' }] })!
  assert.equal(info.schedules[0].lastRun, 1760000000000)
  assert.equal(info.schedules[0].nextRun, Date.parse('2026-10-23T09:00:00Z'))
  assert.equal(info.schedules[0].paused, true)
})

test('cost reads in dollars, with a floor for tiny amounts', () => {
  assert.equal(formatUsd(2.7219), '$2.72')
  assert.equal(formatUsd(0.001), '<$0.01')
  assert.equal(formatUsd(0), '$0.00')
  const top = topModels(parseKithInfo(sample)!.cost!)
  assert.equal(top.length, 3)
  assert.equal(top[0].model, 'claude-sonnet-5-5')
  assert.ok(Math.abs(top.reduce((s, m) => s + m.share, 0) - 1) < 1e-9)
})

test('simple cron reads in words; the rest stays raw', () => {
  assert.equal(cronWords('30 7 * * *'), 'daily at 07:30')
  assert.equal(cronWords('0 18 * * 1-5'), 'weekdays at 18:00')
  assert.equal(cronWords('*/15 * * * *'), 'every 15 minutes')
  assert.equal(cronWords('0 * * * *'), 'hourly')
  assert.equal(cronWords('0 9 1 * *'), '0 9 1 * *')
  assert.equal(cronWords('0 9 * * 1'), '0 9 * * 1')
  assert.equal(cronWords('@daily'), '@daily')
})

test('last run: running, relative, or none', () => {
  const now = 1_000_000_000_000
  const base = { id: 'a', cron: '', task: '', paused: false, nextRun: null }
  assert.equal(lastRunText({ ...base, lastRun: now - 2 * 3600_000, lastStatus: 'done' }, now), '2 h ago')
  assert.equal(lastRunText({ ...base, lastRun: now - 5000, lastStatus: 'running' }, now), 'running')
  assert.equal(lastRunText({ ...base, lastRun: null, lastStatus: null }, now), null)
})

test('package chip tooltip', () => {
  assert.deepEqual(packageNotes({ version: 'abc', remote: 'r', dirty: true, unpushed: true, pinned: 'v1' }), ['r', 'uncommitted changes', 'not pushed', 'pinned to v1'])
  assert.deepEqual(packageNotes({ version: 'abc', remote: null, dirty: false, unpushed: false, pinned: null }), [])
})

test('search answers', () => {
  const search = parseSearchResult({ hits: [{ path: 'a.md', line: 3, snippet: 's', openable: true }, { path: '/abs/skill.md', line: null, snippet: 'x', agent: 'house' }, 'bad'] })
  assert.equal(search.hits.length, 2)
  assert.equal(search.hits[0].openable, true)
  assert.equal(search.hits[1].openable, false)
  assert.equal(search.hits[1].agent, 'house')
  assert.equal(parseSearchResult({ hits: [], error: 'no kith' }).error, 'no kith')
})

test('an older host is told apart from a failure', () => {
  assert.equal(isUnsupportedHost({ status: 404, message: 'x' }), true)
  assert.equal(isUnsupportedHost(new Error('unknown method')), true)
  assert.equal(isUnsupportedHost({ status: 400, message: 'bad' }), false)
})

test('rolePrompt joins the intro and every section, Not yours included', () => {
  const info = parseKithInfo(sample)!
  const text = rolePrompt(info.role)!
  assert.ok(text.startsWith(info.role!.intro.trim()))
  for (const s of info.role!.sections) assert.ok(text.includes(`## ${s.title}`))
  assert.equal(rolePrompt(null), null)
  assert.equal(rolePrompt({ intro: ' ', sections: [] }), null)
})

test('access grants: the host list parsed, and what each lets them do in words', () => {
  const grants = parseAccessGrants({ grants: [
    { person: 'sam', name: 'Sam', kind: 'read', target: '/Users/kai/docs', since: 1_790_000_000_000 },
    { person: '*', name: '', kind: 'domain', target: 'pypi.org', since: null },
    { person: 'sam', kind: 'exec', target: '/bin' },
    { person: 'sam', kind: 'write' },
  ] })
  assert.deepEqual(grants.map((g) => [g.person, g.kind, g.target]), [['sam', 'read', '/Users/kai/docs'], ['*', 'domain', 'pypi.org']])
  assert.equal(accessGrantText(grants[0]), 'Read /Users/kai/docs')
  assert.equal(accessGrantText({ kind: 'write', target: '/x' }), 'Read and write /x')
  assert.equal(accessGrantText(grants[1]), 'Connect to pypi.org')
  assert.match(accessGrantHolder(grants[0]), /^Sam · since /)
  assert.equal(accessGrantHolder(grants[1]), 'Everyone')
  assert.deepEqual(parseAccessGrants(null), [])
})
