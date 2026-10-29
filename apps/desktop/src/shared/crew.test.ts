import assert from 'node:assert/strict'
import { test } from 'node:test'
import { avatarImage, CREW, crewName, SPARES } from './crew.ts'
import { PALETTE, paletteColor, paletteValue, parseColor, rigOf } from './living-bots.ts'

const bot = (id: string, name: string, kind = 'agent', avatarSource?: string) => ({ id, name, kind, avatarSource })

test('a bot named for a crew member takes that crew avatar', () => {
  assert.equal(crewName(bot('b1', 'house')), 'house')
  assert.equal(crewName(bot('b2', 'Coder')), 'coder')
  assert.equal(crewName(bot('b3', 'Resource Manager')), 'resource-manager')
})

test('the bot id matches too, with or without an hq prefix', () => {
  assert.equal(crewName(bot('hq-sound', 'Sound helper')), 'sound')
  assert.equal(crewName(bot('video', 'My bot')), 'video')
})

test('other bots and groups have no crew name', () => {
  assert.equal(crewName(bot('b4', 'Sam')), null)
  assert.equal(crewName(bot('house', 'house', 'group')), null)
})

test('auto shows the matching crew portrait, else the default hammer bot', () => {
  assert.deepEqual(avatarImage(bot('b1', 'house')), { kind: 'crew', name: 'house' })
  assert.deepEqual(avatarImage(bot('b2', 'seo', 'agent', 'auto')), { kind: 'crew', name: 'builder' })
})

test('a host that sends no avatarSource means auto', () => {
  assert.deepEqual(avatarImage({ id: 'b', name: 'Sam', kind: 'agent' }), { kind: 'crew', name: 'builder' })
})

test('an explicit crew or spare image wins over the name', () => {
  assert.deepEqual(avatarImage(bot('b', 'house', 'agent', 'crew:video')), { kind: 'crew', name: 'video' })
  assert.deepEqual(avatarImage(bot('b', 'Sam', 'agent', 'spare:gradcap')), { kind: 'spare', name: 'gradcap' })
})

test('old shape data means auto, groups keep their own avatar', () => {
  assert.deepEqual(avatarImage(bot('b', 'house', 'agent', 'shape')), { kind: 'crew', name: 'house' })
  assert.deepEqual(avatarImage(bot('b', 'Sam', 'agent', 'shape')), { kind: 'crew', name: 'builder' })
  assert.equal(avatarImage(bot('g', 'Room', 'group')), null)
})

test('every crew member and spare has an eye rig, and so does the kith cube', () => {
  for (const name of [...CREW, ...SPARES, 'kith']) {
    const rig = rigOf(name)
    assert.ok(rig, name)
    assert.equal(rig.eyes.length, 2, name)
    for (const e of rig.eyes) assert.ok(e.w > 0 && e.h > 0 && e.x > 0 && e.x < 1 && e.y > 0 && e.y < 1, name)
    assert.ok(rig.look.left > 0 && rig.look.up > 0, name)
  }
  assert.equal(rigOf('kith')!.tint, false)
  assert.equal(rigOf('coder')!.detail, true)
})

test('an avatar colour is a `kith:` palette pick; empty and old ids keep the original body', () => {
  assert.equal(PALETTE.length, 15)
  assert.deepEqual(paletteColor('kith:coral'), PALETTE.find((c) => c.id === 'coral'))
  assert.equal(paletteValue('coral'), 'kith:coral')
  // Old bots carry bare ids, some of which (blue, green, cyan, violet) are also palette names: they must not recolour.
  for (const old of ['', 'black', 'brown', 'red', 'orange', 'yellow', 'magenta', 'gray', 'blue', 'green', 'cyan', 'violet', 'coral', 'kith:nope', 'kith:', undefined]) assert.equal(paletteColor(old), null)
})

test('plain is `kith:<id>:plain`, a flat fill of the pair; gradient and old values are unchanged', () => {
  const coral = PALETTE.find((c) => c.id === 'coral')!
  assert.equal(paletteValue('coral', true), 'kith:coral:plain')
  assert.deepEqual(parseColor('kith:coral'), { pair: coral, plain: false })
  assert.deepEqual(parseColor('kith:coral:plain'), { pair: coral, plain: true })
  assert.deepEqual(paletteColor('kith:coral:plain'), coral)
  for (const none of ['', 'coral', 'blue', 'kith:blue:soft', 'kith:nope:plain', 'kith::plain', 'kith:plain', undefined, null]) assert.equal(parseColor(none), null)
})

test('an image this app does not know falls back to auto', () => {
  assert.deepEqual(avatarImage(bot('b', 'Sam', 'agent', 'crew:nobody')), { kind: 'crew', name: 'builder' })
  assert.deepEqual(avatarImage(bot('b', 'Sam', 'agent', 'spare:nope')), { kind: 'crew', name: 'builder' })
  assert.deepEqual(avatarImage(bot('b', 'Sam', 'agent', 'future:thing')), { kind: 'crew', name: 'builder' })
})
