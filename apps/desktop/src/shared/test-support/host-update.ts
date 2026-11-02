import { EventEmitter } from 'node:events'
import * as crypto from 'node:crypto'
import * as path from 'node:path'
import { identity } from '../environment.ts'
import * as hostService from '../host-service.ts'
import { loadMainModule, settle } from './main-module.ts'

export function hostUpdateFixture(options: { platform?: string; service?: string; devPort?: string; installed?: boolean; missingBinary?: boolean; restartMarker?: boolean } = {}) {
  const platform = options.platform ?? 'darwin'
  const binary = platform === 'win32' ? '/bundle/kith-serve.exe' : platform === 'linux' ? '/home/test/.local/bin/kith-serve' : '/bundle/kith-serve'
  const binaryBytes = Buffer.from('new signed host fixture')
  const persisted: Record<string, unknown> = { hostRestartAfterAppUpdate: options.restartMarker ?? false }
  const calls: string[][] = []
  const order: string[] = []
  const timers = new Map<number, { callback: () => void; ms: number }>()
  let timerId = 0
  const behavior = {
    stopStatus: 0,
    installStatus: 0,
    installed: options.installed ?? true,
    busy: false,
    reachable: true,
    wrongBinary: false,
  }
  const service = () => {
    if (!behavior.installed) throw new Error('No service')
    // A definition the test wrote (a `kith serve start` job) stands as it is; the app's own is just its binary.
    if (options.service) return options.service
    return binary.replace(/kith-serve\.exe$/, identity.supervisor)
  }
  const fs = {
    existsSync: (name: string) => !options.missingBinary && name === binary,
    realpathSync: (name: string) => name,
    readFileSync: (name: string) => name.endsWith('host-prefs.json') ? JSON.stringify(persisted) : service(),
    promises: {
      mkdir: async () => {},
      writeFile: async (_name: string, value: string) => { Object.assign(persisted, JSON.parse(value)) },
      readFile: async (name: string) => name.endsWith('/token') ? 'fixture-token' : binaryBytes,
    },
  }
  const module = loadMainModule<typeof import('../../main/host-controller.ts')>('host-controller', {
    'node:events': { EventEmitter }, 'node:crypto': crypto, 'node:path': path, 'node:fs': fs,
    'node:os': { homedir: () => '/home/test', platform: () => platform },
    'node:child_process': {
      execFileSync: () => service(),
      execFile: (_bin: string, args: string[], _options: unknown, callback: (error: { code: number } | null, stdout: string, stderr: string) => void) => {
        calls.push([...args]); order.push(args[0]!)
        const status = args[0] === 'stop' ? behavior.stopStatus : behavior.installStatus
        callback(status ? { code: status } : null, '', status ? 'service command failed' : '')
      },
    },
    electron: { app: { isPackaged: true, getPath: () => '/prefs' } },
    './environment': { identity },
    '../shared/host-service': hostService,
    './screen': { unregisterScreenAgent: () => { order.push('unregisterScreen') } },
  }, {
    __dirname: '/app/out/main',
    process: { env: options.devPort ? { KITH_SERVE_PORT: options.devPort } : {}, resourcesPath: '/bundle' },
    setTimeout: (callback: () => void, ms: number) => { const id = ++timerId; timers.set(id, { callback, ms }); return id },
    clearTimeout: (id: number) => timers.delete(id),
    fetch: async () => {
      if (!behavior.reachable) throw new Error('Host offline')
      return { ok: true, json: async () => ({ computerId: 'fixture-computer', busy: behavior.busy, binaryPath: binary, binaryHash: behavior.wrongBinary ? 'stale' : crypto.createHash('sha256').update(binaryBytes).digest('hex') }) }
    },
  })
  const host = new module.HostController()
  const tick = async (ms: number) => {
    for (const [id, timer] of [...timers]) {
      if (timer.ms !== ms) continue
      timers.delete(id)
      timer.callback()
    }
    await settle()
  }
  return { host, prefs: module.prefs, persisted, calls, order, behavior, tick }
}
