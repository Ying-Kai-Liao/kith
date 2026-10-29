import assert from 'node:assert/strict'
import test from 'node:test'
import type { Bot, ScreenState } from '@shared/models'
import { chatSubtitle, showsComputerItem, usingComputer } from './chat-subtitle.ts'

const bot = { id: 'b1', computer: true } as unknown as Bot
const screen = { agentBot: 'b1' } as ScreenState
const facts = { bot, isLoopback: true, isOwner: true, screen }
const online = { kind: 'online' } as never

test('the menu item needs an agent with computer use, owned here', () => {
  assert.equal(showsComputerItem(facts), true)
  assert.equal(showsComputerItem({ ...facts, bot: { ...bot, computer: false } as Bot }), false)
  assert.equal(showsComputerItem({ ...facts, bot: { ...bot, botGuest: { name: 'x' } } as unknown as Bot }), false)
  assert.equal(showsComputerItem({ ...facts, isOwner: false }), false)
  assert.equal(showsComputerItem({ ...facts, isLoopback: false }), false)
  assert.equal(showsComputerItem({ ...facts, bot: null }), false)
})

test('idle computer use has the item but no subtitle', () => {
  const idle = { ...facts, screen: { agentBot: null } as ScreenState }
  assert.equal(showsComputerItem(idle), true)
  assert.equal(usingComputer(idle), false)
  assert.equal(chatSubtitle(idle, online, false, 'Mac'), null)
})

test('using the computer shows the subtitle when Live', () => {
  assert.deepEqual(chatSubtitle(facts, online, false, 'Mac'), { kind: 'using' })
  assert.equal(chatSubtitle({ ...facts, bot: { id: 'b2', computer: true } as unknown as Bot }, online, false, 'Mac'), null)
})

test('a chip wins over using the computer', () => {
  assert.deepEqual(chatSubtitle(facts, { kind: 'connecting' } as never, false, 'Mac'), { kind: 'chip', chip: 'catching-up' })
  assert.deepEqual(chatSubtitle(facts, { kind: 'offline' } as never, false, 'Mac mini'), { kind: 'chip', chip: 'mac-unreachable', detail: 'Mac mini' })
  assert.deepEqual(chatSubtitle(facts, online, true, 'Mac'), { kind: 'chip', chip: 'machine', detail: 'Needs update' })
})

test('Live with no computer use shows nothing', () => {
  assert.equal(chatSubtitle({ ...facts, bot: { id: 'b1', computer: false } as unknown as Bot }, online, false, 'Mac'), null)
})
