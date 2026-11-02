import assert from 'node:assert/strict'
import { test } from 'node:test'
import { canInstall, connectorStatusText, installName, parseAvailable, parseMarket, parsePreview, parseTools, removableSkill, skillHolders, targetBots } from './kith-tools.ts'
import type { Bot } from './models.ts'

const bot = (over: Partial<Bot>) => ({ id: 'b', kind: 'agent', name: 'B', hqAgent: 'b', ...over }) as Bot

test('targetBots: the owner\'s kith agents, once each, no groups or guests', () => {
  const got = targetBots([
    bot({ id: '1', name: 'Zed', hqAgent: 'zed' }),
    bot({ id: '2', name: 'Zed again', hqAgent: 'zed' }),
    bot({ id: '3', name: 'Host bot', hqAgent: null }),
    bot({ id: '4', name: 'Room', kind: 'group', hqAgent: 'room' }),
    bot({ id: '5', name: 'Guest', hqAgent: 'g', botGuest: { ownerPerson: 'p' } as Bot['botGuest'] }),
    bot({ id: '6', name: 'Ann', hqAgent: 'ann' }),
  ])
  assert.deepEqual(got.map((b) => b.id), ['6', '1'])
})

test('parseTools keeps a half answer and the error of the other half', () => {
  const t = parseTools({
    connectors: [{ name: 'notion', status: 'signed out', profile: 'notion/host', missing: [], declared: false }, { status: 'ok' }],
    skills: [{ name: 'pdf', kind: 'linked', description: 'd' }],
    connectorsError: 'boom',
  })
  assert.equal(t.connectors.length, 1)
  assert.equal(t.connectors[0].profile, 'notion/host')
  assert.equal(t.connectorsError, 'boom')
  assert.equal(t.skillsError, null)
  assert.deepEqual(parseTools(null), { connectors: [], skills: [], connectorsError: null, skillsError: null })
})

test('status words and what comes off', () => {
  assert.equal(connectorStatusText({ status: 'unknown service', missing: [] }), 'Not installed on this Mac any more')
  assert.equal(connectorStatusText({ status: 'missing secrets', missing: ['KEY'] }), 'Needs KEY')
  assert.equal(removableSkill({ name: 'a', kind: 'linked', description: '' }), true)
  assert.equal(removableSkill({ name: 'a', kind: 'own', description: '' }), false)
})

test('available connectors say which agents have them; skillHolders counts linked skills only', () => {
  assert.deepEqual(parseAvailable([{ name: 'x', connected: ['a', 'b'] }, { connected: [] }]).map((c) => c.connected), [['a', 'b']])
  const m = new Map([
    ['a', parseTools({ skills: [{ name: 'pdf', kind: 'linked' }, { name: 'mine', kind: 'own' }] })],
    ['b', parseTools({ skills: [{ name: 'pdf', kind: 'linked' }] })],
  ])
  const holders = skillHolders(m)
  assert.deepEqual(holders.get('pdf'), ['a', 'b'])
  assert.equal(holders.has('mine'), false)
})

test('parseMarket and parsePreview read the CLI shapes', () => {
  const [m] = parseMarket([{ repo: 'o/kith-agent-x', name: 'x', stars: 3, installed: true, installed_as: 'x2' }, { name: 'no repo' }])
  assert.equal(m.installedAs, 'x2')
  assert.equal(m.stars, 3)
  const p = parsePreview({
    name: 'x', name_taken: true, suggested_name: 'x-2', harness: 'claude',
    needs: [{ service: 'notion', known: true, has_profile: false, granted: false }],
    requires: [{ program: 'ffmpeg', present: false, why: 'video' }],
    skills: { own: ['a'], linked: { pdf: 'anthropics/skills' } },
    permissions: { permission_mode: 'auto', allow: ['Bash(ls)'], hooks: ['x.sh'], mcp: { m: 'cmd' }, tools: ['tools/a.py'] },
    warnings: ['w'],
  })
  assert.equal(p.nameTaken, true)
  assert.deepEqual(p.skills.linked, ['pdf'])
  assert.deepEqual(p.permissions.mcp, ['m: cmd'])
  assert.equal(p.requires[0].present, false)
  assert.deepEqual(p.warnings, ['w'])
})

test('a taken name needs another one before Install', () => {
  const taken = parsePreview({ name: 'x', name_taken: true, suggested_name: 'x-2' })
  assert.equal(installName(taken, ''), 'x-2')
  assert.equal(installName(taken, ' mine '), 'mine')
  assert.equal(canInstall(taken, ''), true)
  assert.equal(canInstall(taken, 'x'), false)
  const free = parsePreview({ name: 'x' })
  assert.equal(installName(free, 'ignored'), null)
  assert.equal(canInstall(free, ''), true)
})
