import assert from 'node:assert/strict'
import { test } from 'node:test'
import { isKithServeService, launchdProgram, restartService, systemdProgram, type CommandRunner } from './host-service.ts'

const plist = (extra = '') => `<plist><dict><key>ProgramArguments</key><array><string>/x/kith-serve</string></array>${extra}</dict></plist>`

test("a service with HQ_BIN is kith serve's, wherever its program is", () => {
  assert.equal(isKithServeService(plist('<key>HQ_BIN</key><string>/b/hq</string>'), '/x/kith-serve', '/Users/me', '.kith/serve'), true)
})

test("a service running the binary kith serve installs is kith serve's", () => {
  assert.equal(isKithServeService(plist(), '/Users/me/.kith/serve/bin/kith-serve', '/Users/me', '.kith/serve'), true)
})

test("the app's own bundled service is not", () => {
  assert.equal(isKithServeService(plist(), '/Applications/Kith.app/Contents/Resources/kith-serve', '/Users/me', '.kith/serve'), false)
  assert.equal(isKithServeService(plist(), null, '/Users/me', '.kith/serve'), false)
})

test('systemd ExecStart: plain, prefixed and quoted programs', () => {
  assert.equal(systemdProgram('[Service]\nExecStart=/usr/bin/kith-serve serve'), '/usr/bin/kith-serve')
  assert.equal(systemdProgram('ExecStart=-/usr/bin/x'), '/usr/bin/x')
  assert.equal(systemdProgram('ExecStart="/home/me/My Tools/kith-serve" serve --port 1'), '/home/me/My Tools/kith-serve')
  assert.equal(systemdProgram("ExecStart='/home/me/My Tools/k' a"), '/home/me/My Tools/k')
  assert.equal(systemdProgram('ExecStart=/home/me/My\\sTools/k a'), '/home/me/My Tools/k')
  assert.equal(systemdProgram('ExecStart="/h/a\\"b\\\\c/k" x'), '/h/a"b\\c/k')
  assert.equal(systemdProgram('ExecStart="/h/a\\x20b/k"'), '/h/a b/k')
  assert.equal(systemdProgram('ExecStart="/h/50%% Tools/$$bin/k" serve'), '/h/50% Tools/$bin/k')
  assert.equal(systemdProgram('[Service]\nRestart=always'), null)
})

test('launchd plist program: XML entities are decoded', () => {
  const doc = (p: string) => `<key>ProgramArguments</key>\n<array>\n<string>${p}</string>\n<string>serve</string></array>`
  assert.equal(launchdProgram(doc('/Users/me/Tom &amp; Jerry/kith-serve')), '/Users/me/Tom & Jerry/kith-serve')
  assert.equal(launchdProgram(doc('/a&#32;b/&lt;x&gt;&quot;&apos;')), '/a b/<x>"\'')
  assert.equal(launchdProgram(doc('/plain/kith-serve')), '/plain/kith-serve')
  assert.equal(launchdProgram('<dict></dict>'), null)
})

function recorder(failing: string[]) {
  const calls: string[] = []
  const run: CommandRunner = async (bin, args) => {
    const line = [bin, ...args].join(' ')
    calls.push(line)
    return { status: failing.some((f) => line.startsWith(f)) ? 1 : 0, output: '' }
  }
  return { calls, run }
}
const base = { uid: 501, label: 'dev.kith.serve', plistPath: '/p/dev.kith.serve.plist', systemdUnit: 'kith-serve.service' }

test('restart on macOS: a loaded job is only kickstarted', async () => {
  const { calls, run } = recorder([])
  assert.equal((await restartService({ ...base, platform: 'mac', run })).status, 0)
  assert.deepEqual(calls, ['launchctl print gui/501/dev.kith.serve', 'launchctl kickstart -k gui/501/dev.kith.serve'])
})

test('restart on macOS: a stopped job is bootstrapped from its existing plist, then kickstarted', async () => {
  const { calls, run } = recorder(['launchctl print'])
  assert.equal((await restartService({ ...base, platform: 'mac', run })).status, 0)
  assert.deepEqual(calls, [
    'launchctl print gui/501/dev.kith.serve',
    'launchctl bootstrap gui/501 /p/dev.kith.serve.plist',
    'launchctl kickstart -k gui/501/dev.kith.serve',
  ])
})

test('restart on macOS: a failed bootstrap is reported without kickstart', async () => {
  const { calls, run } = recorder(['launchctl print', 'launchctl bootstrap'])
  assert.equal((await restartService({ ...base, platform: 'mac', run })).status, 1)
  assert.equal(calls.length, 2)
})

test('restart on Linux: start when inactive, restart when active', async () => {
  const stopped = recorder(['systemctl --user is-active'])
  await restartService({ ...base, platform: 'linux', run: stopped.run })
  assert.equal(stopped.calls.at(-1), 'systemctl --user start kith-serve.service')
  const running = recorder([])
  await restartService({ ...base, platform: 'linux', run: running.run })
  assert.equal(running.calls.at(-1), 'systemctl --user restart kith-serve.service')
})
