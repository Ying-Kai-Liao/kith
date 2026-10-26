import { hostname } from 'node:os'
import { BrowserWindow, ipcMain, type WebContents } from 'electron'
import type { CloudAccess } from '../cloud'
import { paths, receive, type DevicePlatform, type Requests } from '../../shared/cloud-contract.gen.ts'
import type { AccessProgress, AccessTicket, CloudResult, RelayCallError, RelayComputer, RelayState } from '../../shared/relay'
import { accessProgress, relayComputers } from '../../shared/relay'
import { requestAccess } from '../../shared/relay-core/access.ts'
import { RelayError, RelayLink } from '../../shared/relay-core/link.ts'
import { dialWebSocket, probeUpgrade } from './dial'

/**
 * Reaching other Macs over the Kith cloud relay, for every window. One encrypted channel per Mac is kept here in the
 * main process (the keys never reach a window) and shared by all windows that use that Mac. The window sees calls,
 * streams and a state through `window.kith.relay`.
 */

const PLATFORMS: Partial<Record<NodeJS.Platform, DevicePlatform>> = { darwin: 'macos', win32: 'windows', linux: 'linux' }

const failure = (status: number, code: string, message: string): CloudResult<never> => ({ ok: false, error: { status, code, message } })

const asError = (e: unknown): RelayCallError =>
  e instanceof RelayError ? { status: e.status, message: e.message, code: e.code } : { status: 0, message: "Can't reach your computer.", code: 'unreachable' }

export function registerRelay(cloud: CloudAccess) {
  /** The links of the signed-in account; another account starts clean. */
  let owner: string | null = null
  const links = new Map<string, RelayLink>()
  const registered = new Map<string, Promise<CloudResult<unknown>>>()
  const streams = new Map<string, () => void>()
  const asking = new Map<string, { requestId: string | null; cancelled: boolean }>()

  const broadcast = (channel: string, ...args: unknown[]) => {
    for (const w of BrowserWindow.getAllWindows()) if (!w.webContents.isDestroyed()) w.webContents.send(channel, ...args)
  }

  /** The device key is registered with the account the first time a Mac is needed, then not again until sign-in changes. */
  const ensureDevice = async (): Promise<CloudResult<{ userId: string }>> => {
    const device = await cloud.device()
    if (!device) return failure(0, 'signedOut', 'Sign in again to reach your computers.')
    if (owner !== device.userId) {
      for (const l of links.values()) l.close()
      links.clear()
      registered.clear()
      owner = device.userId
    }
    let done = registered.get(device.userId)
    if (!done) {
      done = cloud.request('POST', paths.registerDevice(), { name: hostname(), platform: PLATFORMS[process.platform] ?? 'linux' } satisfies Requests['registerDevice'], true)
      registered.set(device.userId, done)
    }
    const res = await done
    if (!res.ok) {
      registered.delete(device.userId)
      return res
    }
    return { ok: true, value: { userId: device.userId } }
  }

  const linkFor = async (computerId: string, signKey: string): Promise<RelayLink | RelayCallError> => {
    const ensured = await ensureDevice()
    if (!ensured.ok) return { status: ensured.error.status, message: ensured.error.message ?? 'Sign in again.', code: ensured.error.code }
    const device = await cloud.device()
    if (!device) return { status: 0, message: 'Sign in again to reach your computers.', code: 'signedOut' }
    const existing = links.get(computerId)
    if (existing) return existing
    const link = new RelayLink({ cloudURL: device.cloudURL, computerId, hostSignKey: signKey, signer: device.signer, dial: dialWebSocket, probe: probeUpgrade })
    link.onState((state: RelayState) => broadcast('relay:state:change', computerId, state))
    links.set(computerId, link)
    return link
  }

  ipcMain.handle('relay:computers', async (): Promise<CloudResult<RelayComputer[]>> => {
    const ensured = await ensureDevice()
    if (!ensured.ok) return ensured
    const res = await cloud.request('GET', paths.listComputers(), undefined, true)
    if (!res.ok) return res
    return { ok: true, value: relayComputers(res.value) }
  })

  ipcMain.handle('relay:call', async (_e, computerId: string, signKey: string, method: string, body: unknown, timeoutMs: number) => {
    const link = await linkFor(computerId, signKey)
    if (!(link instanceof RelayLink)) return { ok: false, error: link }
    try {
      return { ok: true, value: await link.call(method, body, timeoutMs) }
    } catch (e) {
      return { ok: false, error: asError(e) }
    }
  })

  ipcMain.handle('relay:state', async (_e, computerId: string, signKey: string): Promise<RelayState> => {
    const link = await linkFor(computerId, signKey)
    if (!(link instanceof RelayLink)) return { kind: 'failed', message: link.message, code: link.code, final: false }
    link.start()
    return link.state
  })

  // Streams: the window picks the id; ends are told to it with the same error shape as calls.
  ipcMain.on('relay:stream:open', (e, id: string, computerId: string, signKey: string, sub: string, body: unknown) => {
    const sender: WebContents = e.sender
    const key = `${sender.id}:${id}`
    const send = (channel: string, payload: unknown) => {
      if (!sender.isDestroyed()) sender.send(channel, id, payload)
    }
    let cancel: (() => void) | null = null
    let closed = false
    streams.set(key, () => {
      closed = true
      cancel?.()
    })
    sender.once('destroyed', () => streams.get(key)?.())
    void linkFor(computerId, signKey).then((link) => {
      if (closed) return
      if (!(link instanceof RelayLink)) {
        streams.delete(key)
        return send('relay:stream:end', link)
      }
      cancel = link.subscribe(
        sub,
        body,
        (line) => send('relay:stream:data', line),
        (error) => {
          streams.delete(key)
          send('relay:stream:end', error ? asError(error) : null)
        },
      )
    })
  })
  ipcMain.on('relay:stream:close', (e, id: string) => {
    const key = `${e.sender.id}:${id}`
    streams.get(key)?.()
    streams.delete(key)
  })

  /** After access was granted, a channel that gave up on "access removed" tries again. */
  ipcMain.handle('relay:restart', (_e, computerId: string) => {
    links.get(computerId)?.restart()
  })
  ipcMain.handle('relay:forget', (_e, computerId: string) => {
    links.get(computerId)?.close()
    links.delete(computerId)
  })

  // ---- access requests (§4.2 B): commit, wait for the Mac's nonce, reveal. The device nonce lives only here, in memory,
  // so the cloud can't learn it before the Mac has committed to its own. ----

  ipcMain.handle('relay:requestAccess', async (_e, computerId: string, signKey: string): Promise<CloudResult<AccessTicket>> => {
    const ensured = await ensureDevice()
    if (!ensured.ok) return ensured
    const device = await cloud.device()
    if (!device) return failure(0, 'signedOut', 'Sign in again to reach your computers.')
    const earlier = asking.get(computerId)
    if (earlier) earlier.cancelled = true
    const mine = { requestId: null as string | null, cancelled: false }
    asking.set(computerId, mine)
    try {
      return await requestAccess({
        request: cloud.request,
        signer: device.signer,
        computerId,
        signKey,
        cancelled: () => mine.cancelled,
        created: (id) => (mine.requestId = id),
      })
    } finally {
      if (asking.get(computerId) === mine) asking.delete(computerId)
    }
  })

  ipcMain.handle('relay:accessStatus', async (_e, requestId: string): Promise<CloudResult<AccessProgress>> => {
    if (!/^[A-Za-z0-9_-]{1,128}$/.test(requestId)) return failure(400, 'badRequest', 'Bad request id.')
    const res = await cloud.request('GET', paths.getAccessRequest(requestId), undefined, false)
    if (!res.ok) return res
    return { ok: true, value: accessProgress(receive('AccessStatus', res.value).status) }
  })

  /** Stops waiting for a Mac's nonce (if we still are) and withdraws the request. */
  ipcMain.handle('relay:cancelAccess', async (_e, computerId: string, requestId: string | null) => {
    const waiting = asking.get(computerId)
    if (waiting) waiting.cancelled = true
    const id = requestId ?? waiting?.requestId
    if (id && /^[A-Za-z0-9_-]{1,128}$/.test(id)) await cloud.request('DELETE', paths.cancelAccessRequest(id), undefined, false)
  })
}
