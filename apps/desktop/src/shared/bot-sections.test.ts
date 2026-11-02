import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { ALL_SECTIONS, botSections, callModelLabel, hqSettings, PERMISSION_MODES, permissionLabel, permissionMenu, permissionMode } from './bot-sections.ts'
import { parseKithInfo } from './kith-info.ts'

const house = parseKithInfo(JSON.parse(readFileSync(new URL('./test-support/kith-house-info.json', import.meta.url), 'utf8')))!

test('an hq bot with Kith details hides the cards Kith repeats', () => {
  assert.deepEqual(botSections('house', house), {
    instructions: false, backendPicker: false, harnessMemoryTab: false, kithSettings: false,
  })
})

test('a plain bot keeps every card, whatever Kith says', () => {
  assert.deepEqual(botSections(null, house), ALL_SECTIONS)
  assert.deepEqual(botSections(undefined, house), ALL_SECTIONS)
})

test('an hq bot keeps every card until Kith details are on screen, except the harness notes tab', () => {
  assert.deepEqual(botSections('house', null), { ...ALL_SECTIONS, harnessMemoryTab: false, kithSettings: false })
})

test('a card stays when Kith could not read what it repeats', () => {
  const partial = { ...house, harness: null, errors: { role: 'unreadable' } }
  assert.deepEqual(botSections('house', partial), {
    instructions: true, backendPicker: true, harnessMemoryTab: false, kithSettings: false,
  })
})

test('an hq bot on a host without kithInfo keeps Kith\'s own settings controls', () => {
  assert.equal(botSections('house', null, true).kithSettings, true)
  assert.equal(botSections('house', house, true).kithSettings, true)
  assert.equal(botSections('house', house, false).kithSettings, false)
})

test('hq settings: all but Fast mode are written through to kith, Fast mode is read-only "Set in Kith"', () => {
  const rows = hqSettings({ permissionMode: 'plan', callModel: null, model: 'opus', effort: null, harness: null })
  assert.deepEqual(rows.map((r) => [r.key, r.value, r.editable]), [
    ['permission', 'plan', true], ['callModel', 'default', true], ['harness', 'claude', true], ['model', 'opus', true], ['effort', 'default', true], ['fastMode', 'Off', false],
  ])
  for (const r of rows.filter((r) => !r.editable)) assert.match(r.hint, /^Set in Kith/)
})

test('hq settings: nothing is editable until hq has answered, and no permission set in hq shows the default', () => {
  assert.equal(hqSettings(null)[0].editable, false)
  assert.equal(hqSettings({ permissionMode: null, callModel: 'haiku', model: null, effort: null })[0].value, 'auto')
})

test('hq settings: the call model is written through to hq, and shows hq\'s choice', () => {
  const row = hqSettings({ permissionMode: null, callModel: 'haiku', model: null, effort: null })[1]
  assert.deepEqual([row.key, row.value, row.editable], ['callModel', 'haiku', true])
  assert.equal(callModelLabel('default'), 'Default (sonnet)')
  assert.equal(callModelLabel('default', 'opus'), 'Default (opus)')
  assert.equal(callModelLabel('default', 'own'), "Default (agent's own model)")
  assert.equal(callModelLabel('own'), "Agent's own model")
  assert.equal(callModelLabel('gpt-x'), 'gpt-x')
})

test('a permission mode shows by its label, an unknown one as it is', () => {
  assert.equal(permissionLabel('bypassPermissions'), 'Bypass permissions')
  assert.equal(permissionLabel('somethingNew'), 'somethingNew')
})

test('the picker lists Claude Code\'s six modes only, and the old ask and manual read as default', () => {
  assert.deepEqual(PERMISSION_MODES.map((m) => m.id), ['default', 'acceptEdits', 'plan', 'auto', 'bypassPermissions'])
  assert.equal(permissionMode('ask'), 'default')
  assert.equal(permissionMode('manual'), 'default')
  assert.equal(permissionMode('auto'), 'auto')
  assert.equal(hqSettings({ permissionMode: 'manual', callModel: null, model: null, effort: null })[0].value, 'default')
  assert.equal(permissionLabel('manual'), 'Default')
})

test('the Mode menu follows PERMISSION_MODES with the iPhone wording, and only bypass is risky', () => {
  const menu = permissionMenu()
  assert.deepEqual(menu.map((m) => m.id), PERMISSION_MODES.map((m) => m.id))
  assert.equal(menu[0].detail, 'Asks before edits and commands')
  assert.equal(menu.find((m) => m.id === 'plan')?.label, 'Plan')
  assert.deepEqual(menu.filter((m) => m.risky).map((m) => m.id), ['bypassPermissions'])
})
