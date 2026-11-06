import assert from 'node:assert/strict'
import { test } from 'node:test'
import { newChatMenu } from './new-chat-menu.ts'

test('the new-chat menu lists the four actions in order and runs each', () => {
  const calls: string[] = []
  const menu = newChatMenu({ agent: () => calls.push('agent'), person: () => calls.push('person'), bot: () => calls.push('bot'), group: () => calls.push('group') })
  assert.deepEqual(menu.map((m) => m.title), ['New agent', 'Invite a person', 'Invite a bot', 'New group chat'])
  menu.forEach((m) => m.action())
  assert.deepEqual(calls, ['agent', 'person', 'bot', 'group'])
})

test('Quick chat is in the menu only where the built-in scratch agent is', () => {
  const calls: string[] = []
  const go = { agent: () => {}, person: () => {}, bot: () => {}, group: () => {} }
  const menu = newChatMenu({ ...go, quick: () => calls.push('quick') })
  assert.deepEqual(menu.map((m) => m.title), ['New agent', 'Quick chat', 'Invite a person', 'Invite a bot', 'New group chat'])
  menu[1]!.action()
  assert.deepEqual(calls, ['quick'])
  assert.ok(!newChatMenu(go).some((m) => m.title === 'Quick chat'))
})
