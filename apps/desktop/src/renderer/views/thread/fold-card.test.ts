import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { Entry } from '../../../shared/models.ts'
import { foldOf } from './fold-card.ts'

const entry = (data: Record<string, unknown>): Entry => ({ id: 'e', botId: 'b', kind: 'permission', createdAt: 0, rev: 1, data }) as unknown as Entry
const options = [
  { optionId: 'a', name: 'Allow', kind: 'allow_once' },
  { optionId: 'b', name: 'Always', kind: 'allow_always' },
  { optionId: 'c', name: 'Deny', kind: 'reject_once' },
]
const secret = (card: string, state: string) => entry({ card, status: 'pending', secret: { service: 'Notion', vars: ['K'], profile: 'notion/library', account: 'library', purpose: '', replaces: false, state } })

test('fold: a pending card stays open', () => {
  assert.equal(foldOf(entry({ status: 'pending', title: 'npm test', options })), null)
  assert.equal(foldOf(secret('key', 'pending')), null)
})

test('fold: permission answers in plain words', () => {
  assert.deepEqual(foldOf(entry({ status: 'answered', selected: 'a', title: 'npm test', options })), { ok: true, text: 'Allowed npm test' })
  assert.deepEqual(foldOf(entry({ status: 'answered', selected: 'b', title: 'npm test', options })), { ok: true, text: 'Always allowed npm test' })
  assert.deepEqual(foldOf(entry({ status: 'answered', selected: 'c', title: 'rm -rf x', options })), { ok: false, text: 'Denied rm -rf x' })
})

test('fold: cancelled and expired cards are a red x', () => {
  assert.equal(foldOf(entry({ status: 'cancelled', title: 'npm test', options }))?.ok, false)
  assert.equal(foldOf(entry({ status: 'expired', title: 'npm test', options }))?.text, 'Expired — the agent moved on · npm test')
})

test('fold: key and access cards', () => {
  assert.deepEqual(foldOf(secret('key', 'saved')), { ok: true, text: 'Saved the key for notion/library' })
  assert.deepEqual(foldOf(secret('account', 'saved')), { ok: true, text: 'Allowed Notion · library' })
  assert.deepEqual(foldOf(secret('key', 'denied')), { ok: false, text: 'Not now · Notion key' })
  assert.equal(foldOf(secret('account', 'expired'))?.ok, false)
})
