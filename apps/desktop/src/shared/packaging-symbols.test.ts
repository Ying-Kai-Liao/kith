import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test, type TestContext } from 'node:test'

const verifyBuild = createRequire(import.meta.url)('../../tools/verify-build.cjs')

function fixture(t: TestContext, platform = 'darwin') {
  const projectDir = mkdtempSync(join(tmpdir(), 'kith-symbol-check-'))
  t.after(() => rmSync(projectDir, { recursive: true, force: true }))
  mkdirSync(join(projectDir, 'resources'))
  writeFileSync(join(projectDir, 'resources/account-config.json'), '{}')
  return { electronPlatformName: platform, packager: { projectDir, appInfo: { id: 'com.yingkailiao.kith' } } }
}

test('macOS packaging requires the Apple logo manifest entry and every rendered weight', async (t) => {
  const context = fixture(t)
  await assert.rejects(verifyBuild(context), /ENOENT/)
  const symbols = join(context.packager.projectDir, 'out/renderer/symbols')
  mkdirSync(symbols, { recursive: true })
  writeFileSync(join(symbols, 'manifest.json'), '{}')
  await assert.rejects(verifyBuild(context), /missing the Apple logo/)
  writeFileSync(join(symbols, 'manifest.json'), JSON.stringify({ 'apple.logo': { w: 0.9, h: 1.15 } }))
  await assert.rejects(verifyBuild(context), /ENOENT/)
  for (const weight of ['light', 'regular', 'medium', 'semibold', 'bold', 'black']) {
    writeFileSync(join(symbols, `apple.logo.${weight}.png`), 'fixture')
  }
  await assert.doesNotReject(verifyBuild(context))
})

test('Linux and Windows packaging does not require Apple-only SF Symbols', async (t) => {
  for (const platform of ['linux', 'win32']) {
    await assert.doesNotReject(verifyBuild(fixture(t, platform)))
  }
})

test('macOS packaging fails loudly when a bundled native helper is missing', async (t) => {
  const context = fixture(t) as ReturnType<typeof fixture> & { packager: { config?: unknown } }
  const symbols = join(context.packager.projectDir, 'out/renderer/symbols')
  mkdirSync(symbols, { recursive: true })
  writeFileSync(join(symbols, 'manifest.json'), JSON.stringify({ 'apple.logo': { w: 0.9, h: 1.15 } }))
  for (const weight of ['light', 'regular', 'medium', 'semibold', 'bold', 'black']) {
    writeFileSync(join(symbols, `apple.logo.${weight}.png`), 'fixture')
  }
  context.packager.config = { mac: { extraResources: [{ from: 'build/native/kith-speech', to: 'kith-speech' }] } }
  await assert.rejects(verifyBuild(context), /missing build\/native\/kith-speech/)
  mkdirSync(join(context.packager.projectDir, 'build/native'), { recursive: true })
  writeFileSync(join(context.packager.projectDir, 'build/native/kith-speech'), 'fixture')
  await assert.doesNotReject(verifyBuild(context))
})
