import assert from 'node:assert/strict'
import { test } from 'node:test'
import { botGuestVariant, countLabel, dockAction, dockCallVisible, hashSeed, SKY_PRESETS, skyCropRect, repliesLabel, resumeCommand, skyPatchOf, statusChipOf, writerBannerOf } from './logic.ts'

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

test('the eight sky presets match the iOS list', () => {
  assert.deepEqual(
    SKY_PRESETS.map((p) => `${p.name} ${p.top} ${p.bottom} ${p.zoom} ${p.x} ${p.y} ${p.cloud}`),
    [
      'night #0B1538 #2C468F 3 0 0 45',
      'golden #F29A2E #FFDD94 3 100 0 60',
      'dusk #5A3FA8 #F08A6A 4 90 10 55',
      'dawn #4F86E0 #F5A878 4 5 15 60',
      'noon #1F5FD0 #6DB8F5 4 62 25 70',
      'overcast #6F7A87 #C9D0D6 2 40 0 85',
      'twilight #1C2468 #C8509A 4 10 30 50',
      'mint #2E9E8A #D3F2DE 4 55 5 65',
    ],
  )
  assert.equal(new Set(SKY_PRESETS.map((p) => p.name)).size, 8)
})

test('the canvas crop follows the CSS background-size and -position rules', () => {
  const r = skyCropRect(2000, 1000, { x: 50, y: 0 }, 100)
  assert.deepEqual({ x: r.x, y: r.y + 0, w: r.w, h: r.h }, { x: -160, y: 0, w: 420, h: 210 })
  const z = skyCropRect(2000, 1000, { x: 100, y: 0 }, 100, 3)
  assert.deepEqual({ x: z.x, y: z.y + 0, w: z.w, h: z.h }, { x: -200, y: 0, w: 300, h: 150 })
})
