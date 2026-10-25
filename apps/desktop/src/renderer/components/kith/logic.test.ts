import assert from 'node:assert/strict'
import { test } from 'node:test'
import { botGuestVariant, countLabel, dockAction, dockCallVisible, hashSeed, repliesLabel, resumeCommand, skyPatchOf, statusChipOf, writerBannerOf } from './logic.ts'

test('the sky patch is stable per id, and you are always dawn', () => {
  assert.deepEqual(skyPatchOf('acct_mia'), skyPatchOf('acct_mia'))
  assert.equal(hashSeed('acct_mia'), hashSeed('acct_mia'))
  assert.equal(skyPatchOf('anyone', true).tone, 'dawn')
  const tones = new Set(['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j'].map((id) => skyPatchOf(id).tone))
  assert.ok(tones.size > 1, 'ids spread over the tones')
  const p = skyPatchOf('acct_ren')
  assert.ok(p.x >= 0 && p.x <= 100 && p.y >= 0 && p.y <= 100)
})

test('the count badge hides at 0 and caps at 99+', () => {
  assert.equal(countLabel(0), null)
  assert.equal(countLabel(-2), null)
  assert.equal(countLabel(1), '1')
  assert.equal(countLabel(99), '99')
  assert.equal(countLabel(100), '99+')
  assert.equal(countLabel(1000), '99+')
})

test('the call button hides while typing and returns when the box is empty', () => {
  assert.equal(dockCallVisible({ showsCall: true, hasText: false }), true)
  assert.equal(dockCallVisible({ showsCall: true, hasText: true }), false)
  assert.equal(dockCallVisible({ showsCall: true, hasText: false, hasAttachments: true }), false)
  assert.equal(dockCallVisible({ showsCall: false, hasText: false }), false)
  assert.equal(dockAction({ hasText: false }), 'mic')
  assert.equal(dockAction({ hasText: true }), 'send')
  assert.equal(dockAction({ hasText: false, hasAttachments: true }), 'send')
})

test('the writer banner maps each state to its copy and button', () => {
  const mac = 'Mac mini'
  assert.deepEqual(writerBannerOf({ phase: 'idle', mac }), { icon: 'terminal', title: 'Open in terminal', sentence: 'On Mac mini. Reply there, or take over.', button: 'Take over' })
  assert.equal(writerBannerOf({ phase: 'working', mac }).button, null)
  assert.equal(writerBannerOf({ phase: 'working', mac }).icon, 'dot')
  assert.equal(writerBannerOf({ phase: 'taking', mac }).title, 'Taking over…')
  assert.equal(writerBannerOf({ phase: 'failed', mac }).button, 'Try again')
  assert.equal(writerBannerOf({ phase: 'unreachable', mac }).title, 'Mac mini unreachable')
  assert.equal(writerBannerOf({ phase: 'unreachable', mac }).icon, 'warning')
})

test("take over is only for the terminal's owner", () => {
  const other = writerBannerOf({ phase: 'idle', mac: 'Mac mini', mine: false, person: 'Mia' })
  assert.equal(other.title, "Open in Mia's terminal")
  assert.equal(other.button, null)
  assert.equal(writerBannerOf({ phase: 'failed', mac: 'x', mine: false }).button, null)
})

test('status chips always carry a word', () => {
  for (const k of ['live', 'catching-up', 'needs-you', 'mac-unreachable', 'urgent', 'machine'] as const) assert.ok(statusChipOf(k).word.length > 0)
  assert.equal(statusChipOf('machine', 'Mac mini busy, 1 queued').word, 'Mac mini busy, 1 queued')
  assert.equal(statusChipOf('mac-unreachable', 'Mac mini').word, 'Mac mini unreachable')
})

test('small labels', () => {
  assert.equal(repliesLabel(1), '1 reply')
  assert.equal(repliesLabel(5), '5 replies')
  assert.equal(resumeCommand('coder', '4f2a'), 'kith coder --resume 4f2a')
})

test('a bot guest picture: the G disc for grok, the bubble disc for anything else', () => {
  assert.equal(botGuestVariant('grok'), 'grok')
  assert.equal(botGuestVariant('Grok'), 'grok')
  assert.equal(botGuestVariant('other'), 'other')
  assert.equal(botGuestVariant('gemini'), 'other')
  assert.equal(botGuestVariant(''), 'other')
  assert.equal(botGuestVariant(undefined), 'other')
})
