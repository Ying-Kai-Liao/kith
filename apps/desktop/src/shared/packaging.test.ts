import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { execFileSync } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'
const require = createRequire(import.meta.url)
const dev = require('../../tools/dev-builder.cjs')
const { AppInfo } = require('app-builder-lib/out/appInfo')
const { getWindowsInstallationDirName, getWindowsInstallationAppPackageName } = require('app-builder-lib/out/targets/targetUtil')
const { parse } = require('yaml')
const prod = parse(readFileSync(new URL('../../electron-builder.yml', import.meta.url), 'utf8'))
const metadata = JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8'))

test('Linux packages and Windows installation resources have separate development names', () => {
  const mainInfo = new AppInfo({ config: prod, metadata })
  const devInfo = new AppInfo({ config: dev, metadata: { ...metadata, ...dev.extraMetadata } })
  assert.equal(mainInfo.linuxPackageName, 'kith-desktop')
  assert.equal(devInfo.linuxPackageName, 'kith-desktop-dev')
  assert.notEqual(devInfo.updaterCacheDirName, mainInfo.updaterCacheDirName)
  assert.notEqual(getWindowsInstallationDirName(devInfo, true), getWindowsInstallationDirName(mainInfo, true))
  assert.notEqual(getWindowsInstallationAppPackageName(devInfo.name), getWindowsInstallationAppPackageName(mainInfo.name))
  assert.equal(dev.extraMetadata.desktopName, 'kith-dev.desktop')
})

test('development packaging registers only development callbacks and background helpers', () => {
  assert.equal(dev.appId, 'com.yingkailiao.kith.dev')
  assert.equal(dev.productName, 'Kith Dev')
  assert.equal(dev.publish, null)
  assert.ok(new RegExp(dev.mac.signIgnore[0]).test('KithDevScreen.app/Contents/Helpers/cua-driver'))
  assert.deepEqual(dev.protocols.flatMap((p: { schemes: string[] }) => p.schemes), ['kith-dev', 'com.yingkailiao.kith.dev'])
  assert.deepEqual(dev.mac.extraFiles.map((f: { to: string }) => f.to), [
    'Library/LoginItems/KithDevScreen.app',
    'Library/LaunchAgents/com.yingkailiao.kith.dev.screen.plist',
  ])
  assert.ok(dev.win.extraResources.some((f: { to: string }) => f.to === 'kith-servew.exe'))
  assert.ok(!dev.win.extraResources.some((f: { to: string }) => f.to === 'kith-servew-main.exe'))
  assert.equal(dev.linux.executableName, 'kith-dev')
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
