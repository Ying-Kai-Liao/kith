import assert from 'node:assert/strict'
import { test } from 'node:test'
import { identity } from './environment.ts'

test('the one app uses the one host', () => {
  assert.equal(identity.name, 'Kith')
  assert.equal(identity.appId, 'com.yingkailiao.kith')
  assert.equal(identity.scheme, 'kith')
  assert.equal(identity.dataFolder, '.kith/serve')
  assert.equal(identity.port, 19224)
  assert.equal(identity.hostLabel, 'dev.kith.serve')
  assert.equal(identity.systemdUnit, 'kith-serve.service')
  assert.equal(identity.windowsService, 'KithServe')
  assert.equal(identity.supervisor, 'kith-servew.exe')
  assert.equal(identity.screenLabel, 'com.yingkailiao.kith.screen')
})
