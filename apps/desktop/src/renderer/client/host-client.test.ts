import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { registerHooks } from 'node:module'
import { test } from 'node:test'

registerHooks({ resolve(specifier, context, nextResolve) {
  if (specifier.startsWith('@shared/')) return nextResolve(new URL(`../../shared/${specifier.slice(8)}.ts`, import.meta.url).href, context)
  if (specifier.startsWith('.') && context.parentURL && !/\.[a-z]+$/.test(specifier)) {
    const url = new URL(`${specifier}.ts`, context.parentURL)
    if (existsSync(url)) return nextResolve(url.href, context)
  }
  return nextResolve(specifier, context)
} })

test('unrevokePerson calls the host method of that name with the person id', async () => {
  const { HostClient } = await import('./host-client.ts')
  const sent: unknown[][] = []
  const transport = { call: async (...args: unknown[]) => { sent.push(args); return {} }, stream: () => () => {} }
  await new HostClient(transport).unrevokePerson('sam')
  assert.deepEqual(sent, [['unrevokePerson', { personId: 'sam' }, 60_000]])
})

test('unrevokePerson passes the host refusal through', async () => {
  const { HostClient } = await import('./host-client.ts')
  const transport = { call: async () => { throw new Error('Sam was removed for good.') }, stream: () => () => {} }
  await assert.rejects(new HostClient(transport).unrevokePerson('sam'), /removed for good/)
})
