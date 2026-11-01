import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { execFileSync } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'
import { identity } from './environment.ts'
const require = createRequire(import.meta.url)
const { AppInfo } = require('app-builder-lib/out/appInfo')
const { parse } = require('yaml')
const prod = parse(readFileSync(new URL('../../electron-builder.yml', import.meta.url), 'utf8'))
const metadata = JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8'))

test('the packaged identity matches the app and its host', () => {
  assert.equal(prod.appId, identity.appId)
  assert.equal(prod.productName, identity.name)
  assert.deepEqual(prod.protocols.flatMap((p: { schemes: string[] }) => p.schemes), [identity.scheme, identity.appId])
  assert.equal(new AppInfo({ config: prod, metadata }).linuxPackageName, 'kith-desktop')
  assert.deepEqual(prod.mac.extraFiles.map((f: { to: string }) => f.to), [
    `Library/LoginItems/${identity.screenName}.app`,
    `Library/LaunchAgents/${identity.screenLabel}.plist`,
  ])
  // The Windows sign-in entry starts the supervisor under the name the app expects.
  assert.ok(prod.win.extraResources.some((f: { to: string }) => f.to === identity.supervisor))
})

test('the screen helper agent is the one the app registers', () => {
  const plist = readFileSync(new URL(`../../resources/mac/${identity.screenLabel}.plist`, import.meta.url), 'utf8')
  assert.match(plist, new RegExp(`<string>${identity.screenLabel}</string>`))
  assert.match(plist, new RegExp(`${identity.screenName}\\.app/Contents/MacOS/${identity.screenName}`))
})

test('the Mac app bundles the service binary and the kith release layout', () => {
  const to = (prod.mac.extraResources as { from: string; to: string }[]).map((f) => f.to)
  assert.ok(to.includes('kith-serve'))
  assert.ok(to.includes('kith'))
  // Windows keeps its own entries.
  assert.ok(prod.win.extraResources.some((f: { to: string }) => f.to === 'kith-serve.exe'))
})

test('the staged release layout carries the licenses, NOTICE and third-party notices (Apache-2.0 requires it with redistributions)', () => {
  const dir = mkdtempSync(join(tmpdir(), 'kith-stage-'))
  try {
    const share = join(dir, 'share/kith')
    execFileSync('sh', [fileURLToPath(new URL('../../../../scripts/stage-kernel.sh', import.meta.url)), share])
    for (const f of ['LICENSE', 'NOTICE', 'THIRD_PARTY_NOTICES.md', 'kernel/bin/kith']) assert.ok(existsSync(join(share, f)), f)
    assert.match(readFileSync(join(share, 'NOTICE'), 'utf8'), /Copyright/)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('update-formula.sh fills the version and both checksums, the same way every time', () => {
  const dir = mkdtempSync(join(tmpdir(), 'kith-formula-'))
  try {
    const sums = join(dir, 'SHA256SUMS')
    writeFileSync(sums, `${'a'.repeat(64)}  kith-3.0.0-darwin-arm64.tar.gz\n${'b'.repeat(64)}  kith-3.0.0-darwin-x86_64.tar.gz\n`)
    const script = fileURLToPath(new URL('../../../../scripts/update-formula.sh', import.meta.url))
    const run = (v: string) => execFileSync('sh', [script, v, sums], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] })
    const out = run('v3.0.0')
    assert.equal(out, run('3.0.0'))
    assert.match(out, /version "3\.0\.0"/)
    assert.deepEqual(out.match(/sha256 "\w+"/g), [`sha256 "${'a'.repeat(64)}"`, `sha256 "${'b'.repeat(64)}"`])
    assert.throws(() => run('9.9.9'))
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
