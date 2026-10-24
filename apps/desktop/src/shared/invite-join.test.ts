import assert from 'node:assert/strict'
import { test } from 'node:test'
import { inviteProof } from './invite.ts'
import { runJoin, type HostCaller, type JoinDeps, type JoinState } from './invite-join.ts'

const link = { computerId: 'cmp_1', inviteId: 'inv_1', token: 'x'.repeat(24) }

function fake(over: Partial<JoinDeps> = {}) {
  const log: string[] = []
  const sent: { accept: string[]; host: { method: string; body: unknown }[]; kept: string[] } = { accept: [], host: [], kept: [] }
  const call: HostCaller = async (method, body) => {
    sent.host.push({ method, body })
    return { proven: true }
  }
  const deps: JoinDeps = {
    signedIn: () => true,
    waitForSignIn: async () => void log.push('signIn'),
    accept: async (_id, h1) => (sent.accept.push(h1), { computerId: 'cmp_1' }),
    requestAccess: async () => void log.push('ask'),
    connect: async () => call,
    keepToken: async (c, _i, t) => void sent.kept.push(`${c}:${t}`),
    ...over,
  }
  const steps: JoinState[] = []
  return { deps, log, sent, steps, run: () => runJoin(link, deps, (s) => steps.push(s)) }
}

test('joins in order and proves the token to the Mac, sending the cloud only h1', async () => {
  const f = fake()
  const end = await f.run()
  assert.equal(end.step, 'done')
  assert.deepEqual(f.steps.map((s) => s.step), ['joining', 'asking', 'connecting', 'proving', 'done'])
  assert.deepEqual(f.sent.accept, [await inviteProof(link.token)])
  assert.ok(!f.sent.accept.includes(link.token))
  assert.deepEqual(f.sent.host, [{ method: 'proveInvite', body: { inviteId: 'inv_1', token: link.token } }])
  assert.deepEqual(f.sent.kept, [`cmp_1:${link.token}`])
})

test('an accept answer naming another computer fails the join before the token goes anywhere', async () => {
  const f = fake({ accept: async () => ({ computerId: 'cmp_evil' }) })
  const end = await f.run()
  assert.equal(end.step, 'failed')
  assert.equal(end.computerId, 'cmp_1')
  assert.deepEqual(f.sent.kept, [])
  assert.deepEqual(f.sent.host, [])
  assert.deepEqual(f.log, [])
})

test('an accept answer without a computer id uses the link\'s', async () => {
  const f = fake({ accept: async () => ({ computerId: '' }) })
  assert.equal((await f.run()).computerId, 'cmp_1')
})

test('signs in first when signed out', async () => {
  const f = fake({ signedIn: () => false })
  assert.equal((await f.run()).step, 'done')
  assert.equal(f.steps[0]!.step, 'signingIn')
  assert.deepEqual(f.log, ['signIn', 'ask'])
})

test('a cancelled sign-in fails without calling the cloud', async () => {
  const f = fake({ signedIn: () => false, waitForSignIn: async () => { throw new Error('Sign-in was cancelled.') } })
  const end = await f.run()
  assert.equal(end.step, 'failed')
  assert.equal(f.sent.accept.length, 0)
})

for (const [status, message] of [
  [403, 'This is your own computer'],
  [404, 'This invite doesn\'t exist'],
  [409, 'This invite was already used'],
  [410, 'This invite has expired'],
] as const) {
  test(`accept ${status} shows the cloud's words and stops`, async () => {
    const f = fake({ accept: async () => { throw { status, code: 'x', message } } })
    const end = await f.run()
    assert.deepEqual([end.step, end.message], ['failed', message])
    assert.deepEqual(f.log, [])
    assert.equal(f.sent.host.length, 0)
  })
}

test('an unreachable cloud has a plain message', async () => {
  const f = fake({ accept: async () => { throw { status: 0, code: 'unreachable', message: null } } })
  assert.match((await f.run()).message ?? '', /Can't reach the Kith cloud/)
})

test('an offline Mac keeps the join waiting, and a rerun proves after the reconnect', async () => {
  let online = false
  const f = fake()
  const call: HostCaller = async (m, b) => (f.sent.host.push({ method: m, body: b }), { proven: true })
  f.deps.connect = async () => (online ? call : null)
  const first = await runJoin(link, f.deps, () => {})
  assert.equal(first.step, 'waiting')
  assert.equal(f.sent.host.length, 0)
  online = true
  const second = await runJoin(link, f.deps, () => {})
  assert.equal(second.step, 'done')
  assert.equal(f.sent.host.length, 1)
  // The same-user re-accept is the cloud's to answer: the app just asks again.
  assert.equal(f.sent.accept.length, 2)
})

test('a connection that drops while proving waits; a refusal fails with the Mac\'s words', async () => {
  const dropped = fake({ connect: async () => async () => { throw { status: 0, message: 'gone' } } })
  assert.equal((await dropped.run()).step, 'waiting')
  const refused = fake({ connect: async () => async () => { throw new Error("That isn't this invite's token.") } })
  const end = await refused.run()
  assert.deepEqual([end.step, end.message], ['failed', "That isn't this invite's token."])
  const limited = fake({ connect: async () => async () => { throw new Error('Too many tries. Wait a few minutes.') } })
  assert.equal((await limited.run()).message, 'Too many tries. Wait a few minutes.')
})

test('a device that already proved counts as done', async () => {
  const f = fake({ connect: async () => async () => { throw new Error('This device has already shown its invite.') } })
  assert.equal((await f.run()).step, 'done')
})

test('an app that cannot connect says so', async () => {
  const f = fake({ connect: async () => { throw new Error('unsupported here') } })
  const end = await f.run()
  assert.deepEqual([end.step, end.message], ['failed', 'unsupported here'])
})

test('no safe place for the token stops the join before asking the Mac', async () => {
  const f = fake({ keepToken: async () => { throw new Error("Can't store the invite safely on this computer.") } })
  const end = await f.run()
  assert.deepEqual([end.step, end.message], ['failed', "Can't store the invite safely on this computer."])
  assert.deepEqual(f.log, [])
})
