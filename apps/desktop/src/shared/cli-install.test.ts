import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import { cliNoticeFromHello, compareVersions, decide, installedVersion, machineDir, parseVersion, plistSetsKernel, runInstaller } from './cli-install.ts'

test('versions parse and compare numerically', () => {
  assert.deepEqual(parseVersion('v1.2.3\n'), [1, 2, 3])
  assert.deepEqual(parseVersion('0.10.0-rc.1'), [0, 10, 0])
  assert.equal(parseVersion('1.2'), null)
  assert.equal(parseVersion(''), null)
  assert.ok(compareVersions('0.10.0', '0.9.9') > 0)
  assert.equal(compareVersions('0.1.0', '0.1.0'), 0)
  assert.ok(compareVersions('0.1.2', '0.1.10') < 0)
  assert.ok(compareVersions(null, '0.0.1') < 0)
})

test('the declared CLI requirement is the app major.minor.0, like the host MIN_CLI', () => {
  const pkg = JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8'))
  const [major, minor] = pkg.version.split('.')
  assert.equal(pkg.kith.cli, `${major}.${minor}.0`)
})

const facts = { dev: false, required: '0.1.0', installed: null as string | null, bundled: '0.1.3' as string | null }

test('decision: missing, older, equal, newer, dev', () => {
  assert.deepEqual(decide(facts), { action: 'install' })
  assert.deepEqual(decide({ ...facts, installed: '0.0.9' }), { action: 'install' })
  assert.deepEqual(decide({ ...facts, installed: '0.1.0' }), { action: 'skip', why: 'satisfied' })
  assert.deepEqual(decide({ ...facts, installed: '0.2.0' }), { action: 'skip', why: 'satisfied' })
  assert.deepEqual(decide({ ...facts, dev: true }), { action: 'skip', why: 'dev' })
})

test('decision: never downgrades, and needs a bundle', () => {
  assert.deepEqual(decide({ ...facts, required: '0.3.0', installed: '0.2.0', bundled: '0.1.3' }), { action: 'skip', why: 'bundleNotNewer' })
  assert.deepEqual(decide({ ...facts, bundled: null }), { action: 'skip', why: 'noBundle' })
})

test('machine dir mirrors the host', () => {
  const home = '/h'
  const input = (exists: string[], real: string[], state?: string) => ({ state, home, exists: (p: string) => exists.includes(p), isRealDir: (p: string) => real.includes(p) })
  assert.equal(machineDir(input([], [], '/s')), '/s')
  assert.equal(machineDir(input(['/h/.kith/.migrated'], ['/h/.hq'])), '/h/.kith')
  assert.equal(machineDir(input([], ['/h/.hq'])), '/h/.hq')
  assert.equal(machineDir(input([], [])), '/h/.kith')
})

test('installed version: current layout first, old flat layout, else none', () => {
  const files: Record<string, string> = {}
  const read = (p: string) => files[p] ?? null
  assert.equal(installedVersion('/m', read), null)
  files['/m/share/kith/kernel/VERSION'] = '0.0.5\n'
  assert.equal(installedVersion('/m', read), '0.0.5')
  files['/m/cli/current/share/kith/kernel/VERSION'] = '0.1.3\n'
  assert.equal(installedVersion('/m', read), '0.1.3')
  files['/m/cli/current/share/kith/kernel/VERSION'] = 'junk'
  assert.equal(installedVersion('/m', read), '0.0.5')
})

test('a plist with KITH_KERNEL is a dev kernel', () => {
  assert.equal(plistSetsKernel('<key>EnvironmentVariables</key><dict><key>KITH_KERNEL</key><string>/src/kernel</string></dict>'), true)
  assert.equal(plistSetsKernel('<key>HQ_KERNEL</key>\n<string></string>'), false)
  assert.equal(plistSetsKernel('<key>HQ_BIN</key><string>/x</string>'), false)
  assert.equal(plistSetsKernel(null), false)
})

test('the installer runs with the unattended environment; success, failure, timeout', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'cli-install-'))
  const ok = join(dir, 'ok.sh')
  writeFileSync(ok, 'echo "$KITH_INSTALL_CLI_ONLY $KITH_INSTALL_YES $KITH_INSTALL_DIR"\n')
  const good = await runInstaller({ script: ok, dir: '/release' }, {})
  assert.equal(good.ok, true)
  assert.equal(good.output, '1 1 /release')
  const bad = join(dir, 'bad.sh')
  writeFileSync(bad, 'echo noise\necho "install: no space left" >&2\nexit 1\n')
  const failed = await runInstaller({ script: bad, dir: '/release' }, {})
  assert.equal(failed.ok, false)
  assert.equal(failed.message, 'install: no space left')
  const slow = join(dir, 'slow.sh')
  writeFileSync(slow, 'sleep 5\n')
  const timedOut = await runInstaller({ script: slow, dir: '/release' }, {}, 200)
  assert.equal(timedOut.ok, false)
  assert.match(timedOut.message, /didn't finish/)
})

test('hello.cli: a notice only when the host refuses', () => {
  assert.equal(cliNoticeFromHello(undefined), null)
  assert.equal(cliNoticeFromHello({ ok: true }), null)
  assert.match(cliNoticeFromHello({ ok: false, version: null, min: '0.1.0' }) ?? '', /not installed.*0\.1\.0/)
  assert.match(cliNoticeFromHello({ ok: false, version: '0.0.9', min: '0.1.0' }) ?? '', /0\.0\.9 is too old/)
})
