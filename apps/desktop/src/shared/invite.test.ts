import assert from 'node:assert/strict'
import { test } from 'node:test'
import { inviteProof, isInviteProof, parseInviteLink } from './invite.ts'
import { pagesFor, parsePeople, isOpenInvite } from './people.ts'

const schemes = ['kith', 'dev.kith.app']
const TOKEN = 'a'.repeat(43)

test('h1 is the lowercase hex sha256 of the token as UTF-8', async () => {
  assert.equal(await inviteProof('test'), '9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08')
  assert.ok(isInviteProof(await inviteProof('anything')))
  assert.ok(!isInviteProof('ABC'))
})

test('an invite link gives its three parts', () => {
  const link = parseInviteLink(`kith://invite?c=cmp_1&i=inv_ab12&t=${TOKEN}`, schemes)
  assert.deepEqual(link, { computerId: 'cmp_1', inviteId: 'inv_ab12', token: TOKEN })
  assert.ok(parseInviteLink('KITH://INVITE?c=a&i=b&t=12345678', schemes))
})

test('a pasted page link gives the same parts, token from the fragment only', () => {
  const want = { computerId: 'cmp_1', inviteId: 'inv_ab12', token: TOKEN }
  assert.deepEqual(parseInviteLink(`https://kithbot.app/i/cmp_1/inv_ab12#t=${TOKEN}`, schemes), want)
  assert.deepEqual(parseInviteLink(`https://other.example/i/cmp_1/inv_ab12/#t=${TOKEN}`, schemes), want)
  for (const url of [
    'https://kithbot.app/i/cmp_1/inv_ab12',
    `https://kithbot.app/i/cmp_1/inv_ab12?t=${TOKEN}`,
    `https://kithbot.app/i/cmp_1#t=${TOKEN}`,
    `https://kithbot.app/x/cmp_1/inv_ab12#t=${TOKEN}`,
    `http://kithbot.app/i/cmp_1/inv_ab12#t=${TOKEN}`,
    'https://kithbot.app/i/cmp_1/inv_ab12#t=short',
  ]) assert.equal(parseInviteLink(url, schemes), null, url)
})

test('bad or missing parts are rejected', () => {
  for (const url of [
    'kith://invite?c=a&i=b',
    'kith://invite?c=a&t=12345678',
    'kith://invite?i=b&t=12345678',
    'kith://invite?c=&i=b&t=12345678',
    'kith://invite?c=a&i=b&t=short',
    'kith://invite?c=a b&i=b&t=12345678',
    'kith://invite?c=../x&i=b&t=12345678',
    'kith://callback?c=a&i=b&t=12345678',
    'https://invite?c=a&i=b&t=12345678',
    'not a url',
    '',
  ]) assert.equal(parseInviteLink(url, schemes), null, url)
})

test('only the owner sees the owner pages', () => {
  const all = ['general', 'computers', 'people', 'voice', 'updates'] as const
  const ownerOnly = ['computers', 'people', 'voice', 'updates'] as const
  assert.deepEqual(pagesFor('owner', all, ownerOnly), [...all])
  assert.deepEqual(pagesFor('member', all, ownerOnly), ['general'])
})

test('the people list is read defensively', () => {
  const people = parsePeople({ people: [{ personId: 'sam', name: 'Sam', agents: { house: 'member', money: 'x' }, joined: true, devices: [{ key: 'k', lastSeenAt: 5 }] }, {}] })
  assert.equal(people[0]!.agents.house, 'member')
  assert.equal(people[0]!.agents.money, 'guest')
  assert.equal(people[0]!.devices[0]!.lastSeenAt, 5)
  assert.equal(people.length, 2)
  assert.deepEqual(parsePeople(null), [])
  const revoked = parsePeople({ people: [{ personId: 'a', revoked: true }, { personId: 'b' }, { personId: 'c', revoked: 'yes' }] })
  assert.deepEqual(revoked.map((p) => p.revoked), [true, false, false])
  assert.ok(isOpenInvite({ inviteId: 'i', personId: 'p', agents: {}, createdAt: 0, expiresAt: 10, usedAt: null, revokedAt: null }, 5))
  assert.ok(!isOpenInvite({ inviteId: 'i', personId: 'p', agents: {}, createdAt: 0, expiresAt: 10, usedAt: 1, revokedAt: null }, 5))
})
