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
