import { contextBridge, ipcRenderer, type IpcRendererEvent } from 'electron'
import { sshBridge } from './ssh'
import type { CallError, KithBridge, HostSnapshot, TraySummary, WindowCommand } from '../shared/ipc'
import type { RelayCallError } from '../shared/relay'

let streamId = 0
const streams = new Map<string, { onData: (line: string) => void; onEnd: (error: CallError | null) => void }>()

ipcRenderer.on('host:stream:data', (_e, id: string, line: string) => streams.get(id)?.onData(line))
ipcRenderer.on('host:stream:end', (_e, id: string, error: CallError | null) => {
  const s = streams.get(id)
  streams.delete(id)
  s?.onEnd(error)
})

const relayStreams = new Map<string, { onData: (line: string) => void; onEnd: (error: RelayCallError | null) => void }>()

ipcRenderer.on('relay:stream:data', (_e, id: string, line: string) => relayStreams.get(id)?.onData(line))
ipcRenderer.on('relay:stream:end', (_e, id: string, error: RelayCallError | null) => {
  const s = relayStreams.get(id)
  relayStreams.delete(id)
  s?.onEnd(error)
})

function listen<T>(channel: string, cb: (value: T) => void) {
  const handler = (_e: IpcRendererEvent, value: T) => cb(value)
  ipcRenderer.on(channel, handler)
  return () => void ipcRenderer.removeListener(channel, handler)
}

const bridge: KithBridge = {
  platform: process.platform as KithBridge['platform'],
  appName: ipcRenderer.sendSync('app:name') as string,
  appScheme: ipcRenderer.sendSync('app:scheme') as string,
  hostPort: ipcRenderer.sendSync('app:hostPort') as number,
  appVersion: ipcRenderer.sendSync('app:version') as string,
  computerName: ipcRenderer.sendSync('app:computerName') as string,
  debugOpen: ipcRenderer.sendSync('app:debugOpen') as string | null,
  host: {
    snapshot: () => ipcRenderer.invoke('host:snapshot') as Promise<HostSnapshot>,
    onChange: (cb) => listen('host:change', cb),
    install: () => ipcRenderer.send('host:install'),
    restart: () => ipcRenderer.send('host:restart'),
    async call(baseURL, token, method, body, timeoutMs) {
      const result = (await ipcRenderer.invoke('host:call', baseURL, token, method, body, timeoutMs)) as
        | { ok: true; value: unknown }
        | { ok: false; error: CallError }
      if (!result.ok) throw result.error
      return result.value
    },
    stream(url, token, onData, onEnd) {
      const id = `s${++streamId}`
      streams.set(id, { onData, onEnd })
      ipcRenderer.send('host:stream:open', id, url, token)
      return () => {
        if (!streams.delete(id)) return
        ipcRenderer.send('host:stream:close', id)
      }
    },
  },
  account: {
    state: () => ipcRenderer.invoke('account:state'),
    onChange: (cb) => listen('account:change', cb),
    signIn: (provider) => ipcRenderer.invoke('account:signIn', provider),
    signOut: () => ipcRenderer.invoke('account:signOut'),
    setProfileImage: (source) => ipcRenderer.invoke('account:setProfileImage', source),
    token: () => ipcRenderer.invoke('account:token'),
  },
  speech: {
    available: ipcRenderer.sendSync('speech:available') as boolean,
    locales: () => ipcRenderer.sendSync('speech:locales') as string[],
    start: (locale, options) => ipcRenderer.send('speech:start', locale, options),
    stop: () => ipcRenderer.send('speech:stop'),
    onEvent: (cb) => listen('speech:event', cb),
  },
  ssh: sshBridge,
  incomingCall: {
    show: (call) => ipcRenderer.invoke('incomingCall:show', call),
    close: (id) => ipcRenderer.send('incomingCall:close', id),
    onAnswer: (cb) => listen('incomingCall:answer', cb),
    current: () => ipcRenderer.invoke('incomingCall:current'),
    onCall: (cb) => listen('incomingCall:call', cb),
    respond: (id, answer) => ipcRenderer.send('incomingCall:respond', id, answer),
  },
  island: {
    supported: ipcRenderer.sendSync('island:supported') as boolean,
    enabled: () => ipcRenderer.invoke('island:enabled'),
    setEnabled: (on) => ipcRenderer.invoke('island:setEnabled', on),
    setSummary: (summary) => ipcRenderer.send('island:summary', summary),
    state: () => ipcRenderer.invoke('island:state'),
    onSummary: (cb) => listen('island:summary', cb),
    onScreen: (cb) => listen('island:screen', cb),
    onHover: (cb) => listen('island:hover', cb),
    setShape: (rect) => ipcRenderer.send('island:shape', rect),
    setFocusable: (on) => ipcRenderer.send('island:focusable', on),
    setKeys: (on) => ipcRenderer.send('island:keys', on),
    onKey: (cb) => listen('island:key', cb),
    respond: (ref, entryId, optionId) => ipcRenderer.send('island:respond', ref, entryId, optionId),
    send: (ref, text) => ipcRenderer.send('island:send', ref, text),
    openBot: (ref, thread) => ipcRenderer.send('island:openBot', ref, thread),
    newBot: () => ipcRenderer.send('island:newBot'),
    openApp: () => ipcRenderer.send('island:openApp'),
  },
  cloud: {
    request: (method, path, body, signed) => ipcRenderer.invoke('cloud:request', method, path, body, signed),
    acceptInvite: (inviteId, h1) => ipcRenderer.invoke('cloud:acceptInvite', inviteId, h1),
  },
  relay: {
    computers: () => ipcRenderer.invoke('relay:computers'),
    async call(computerId, signKey, method, body, timeoutMs) {
      const result = (await ipcRenderer.invoke('relay:call', computerId, signKey, method, body, timeoutMs)) as
        | { ok: true; value: unknown }
        | { ok: false; error: RelayCallError }
      if (!result.ok) throw result.error
      return result.value
    },
    stream(computerId, signKey, sub, body, onData, onEnd) {
      const id = `r${++streamId}`
      relayStreams.set(id, { onData, onEnd })
      ipcRenderer.send('relay:stream:open', id, computerId, signKey, sub, body)
      return () => {
        if (!relayStreams.delete(id)) return
        ipcRenderer.send('relay:stream:close', id)
      }
    },
    state: (computerId, signKey) => ipcRenderer.invoke('relay:state', computerId, signKey),
    onState(cb) {
      const handler = (_e: IpcRendererEvent, computerId: string, state: Parameters<typeof cb>[1]) => cb(computerId, state)
      ipcRenderer.on('relay:state:change', handler)
      return () => void ipcRenderer.removeListener('relay:state:change', handler)
    },
    restart: (computerId) => ipcRenderer.invoke('relay:restart', computerId),
    forget: (computerId) => ipcRenderer.invoke('relay:forget', computerId),
    requestAccess: (computerId, signKey) => ipcRenderer.invoke('relay:requestAccess', computerId, signKey),
    accessStatus: (requestId) => ipcRenderer.invoke('relay:accessStatus', requestId),
    cancelAccess: (computerId, requestId) => ipcRenderer.invoke('relay:cancelAccess', computerId, requestId),
  },
  invites: {
    takePending: () => ipcRenderer.invoke('invite:takePending'),
    keep: (computerId, inviteId, token) => ipcRenderer.invoke('invite:keep', computerId, inviteId, token),
    has: (computerId) => ipcRenderer.invoke('invite:has', computerId),
    kept: (computerId) => ipcRenderer.invoke('invite:kept', computerId),
    forget: (computerId) => ipcRenderer.invoke('invite:forget', computerId),
  },
  updates: {
    state: () => ipcRenderer.invoke('updates:state'),
    onChange: (cb) => listen('updates:change', cb),
    check: () => ipcRenderer.send('updates:check'),
    install: () => ipcRenderer.send('updates:install'),
    setAutoCheck: (on) => ipcRenderer.send('updates:autoCheck', on),
    setAutoDownload: (on) => ipcRenderer.send('updates:autoDownload', on),
  },
  files: {
    begin: (file) => ipcRenderer.invoke('files:begin', file),
    write: (id, offset, data) => ipcRenderer.invoke('files:write', id, offset, data),
    finish: (id) => ipcRenderer.invoke('files:finish', id),
    cancel: (id) => ipcRenderer.invoke('files:cancel', id),
  },
  app: {
    reportError: (report) => ipcRenderer.send('app:reportError', report),
    openExternal: (url) => ipcRenderer.send('app:openExternal', url),
    openMemoryFile: (path) => ipcRenderer.send('app:openMemoryFile', path),
    copy: (text) => ipcRenderer.send('app:copy', text),
    pickFiles: () => ipcRenderer.invoke('app:pickFiles'),
    pickFolder: (defaultPath) => ipcRenderer.invoke('app:pickFolder', defaultPath ?? null),
    readClipboardFiles: () => ipcRenderer.invoke('app:clipboardFiles'),
    setTraySummary: (summary: TraySummary) => ipcRenderer.send('app:traySummary', summary),
    raiseWindow: () => ipcRenderer.send('app:raiseWindow'),
    onCommand: (cb: (c: WindowCommand) => void) => listen('app:command', cb),
    resetAllData: () => ipcRenderer.invoke('app:resetAllData'),
    setLaunchAtLogin: (on) => ipcRenderer.invoke('app:setLaunchAtLogin', on),
    showInDock: () => ipcRenderer.invoke('app:showInDock'),
    setShowInDock: (on) => ipcRenderer.invoke('app:setShowInDock', on),
    setAppearance: (appearance) => ipcRenderer.send('app:setAppearance', appearance),
    launchAtLogin: () => ipcRenderer.invoke('app:launchAtLogin'),
    openSettings: (url) => ipcRenderer.send('app:openSettings', url),
    openSettingsWindow: (page) => ipcRenderer.send('app:openSettingsWindow', page ?? null),
    profileMenu: (menu) => ipcRenderer.invoke('app:profileMenu', menu),
    authenticate: (url, scheme) => ipcRenderer.invoke('app:authenticate', url, scheme),
    screenAgentState: () => ipcRenderer.invoke('screen:state'),
    setScreenAgent: (on) => ipcRenderer.invoke('screen:setAgent', on),
    syncScreenAgent: (enabled) => ipcRenderer.invoke('screen:syncAgent', enabled),
  },
}

contextBridge.exposeInMainWorld('kith', bridge)
