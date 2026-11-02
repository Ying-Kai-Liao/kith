import assert from 'node:assert/strict'
import { test } from 'node:test'
import { appleSignInAvailable, providerName, providerStrategy, resolveAccountConfig } from './account-config.ts'

const file = { clerkPublishableKey: 'pk_test_file', cloudURL: 'https://file.example/' }

test('KITH_* wins over the build file', () => {
  const env = { KITH_CLERK_PUBLISHABLE_KEY: 'pk_live_kith', KITH_CLOUD_URL: 'https://kith.example/' }
  assert.deepEqual(resolveAccountConfig(env, file), { clerkPublishableKey: 'pk_live_kith', cloudURL: 'https://kith.example' })
  assert.deepEqual(resolveAccountConfig({}, file), { clerkPublishableKey: 'pk_test_file', cloudURL: 'https://file.example' })
})

test('empty values fall through, and an empty build config is disabled', () => {
  assert.deepEqual(resolveAccountConfig({ KITH_CLERK_PUBLISHABLE_KEY: '  ', KITH_CLOUD_URL: '' }, file), { clerkPublishableKey: 'pk_test_file', cloudURL: 'https://file.example' })
  assert.deepEqual(resolveAccountConfig({}, { clerkPublishableKey: '', cloudURL: '' }), { clerkPublishableKey: null, cloudURL: null })
  assert.deepEqual(resolveAccountConfig({}, {}), { clerkPublishableKey: null, cloudURL: null })
})

test('a malformed key or a non-https cloud is disabled', () => {
  assert.deepEqual(resolveAccountConfig({ KITH_CLERK_PUBLISHABLE_KEY: 'sk_live_x', KITH_CLOUD_URL: 'http://x.example' }, {}), { clerkPublishableKey: null, cloudURL: null })
})

test('GitHub maps to the oauth_github strategy', () => {
  assert.equal(providerStrategy('github'), 'oauth_github')
  assert.equal(providerStrategy('google'), 'oauth_google')
  assert.equal(providerName('github'), 'GitHub')
})

test('Apple maps to oauth_apple and shows only on a main build with a production key', () => {
  assert.equal(providerStrategy('apple'), 'oauth_apple')
  assert.equal(appleSignInAvailable('pk_live_x'), true)
  assert.equal(appleSignInAvailable('pk_test_x'), false)
  assert.equal(appleSignInAvailable(null), false)
})
