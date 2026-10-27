import { app, crashReporter, ipcMain } from 'electron'
import fs from 'node:fs'
import path from 'node:path'
import { errorReport, mainCrashed, NATIVE_CRASH, type ErrorReport } from '../shared/error-report'
import type { HostController } from './host-controller'

/** Not crashes: a helper that exited normally, or was ended from outside (logout, shutdown, an update). */
const ENDED_FROM_OUTSIDE = new Set(['clean-exit', 'killed'])

/**
 * Error tracking: uncaught errors in the main process and the windows, and windows or helpers
 * that crashed, go to the host's loopback `reportError` (shared/error-report.ts).
 */
export function registerErrorReports(host: HostController) {
  const send = (report: ErrorReport) => {
    host.call('reportError', report).catch(() => {})
  }
  // The monitor only watches: Electron still handles the error as before.
  process.on('uncaughtExceptionMonitor', (error) => send(errorReport(error, 'desktop-main', 'uncaught')))
  process.on('unhandledRejection', (reason) => {
    console.error('Unhandled rejection in the main process:', reason)
    send(errorReport(reason, 'desktop-main', 'unhandledrejection'))
  })
  ipcMain.on('app:reportError', (_e, report: ErrorReport) => send({ ...report, process: 'desktop-renderer' }))
  app.on('render-process-gone', (_e, _contents, details) => {
    if (!ENDED_FROM_OUTSIDE.has(details.reason)) send({ process: 'desktop-main', type: 'RendererGone', mechanism: 'render-process-gone', value: details.reason, frames: [] })
  })
  app.on('child-process-gone', (_e, details) => {
    // A GPU process that can't start (Linux without GPU access) only means software rendering: the app goes on.
    const softwareRendering = details.type === 'GPU' && details.reason === 'launch-failed'
    if (!ENDED_FROM_OUTSIDE.has(details.reason) && !softwareRendering) send({ process: 'desktop-main', type: `${details.type}Gone`, mechanism: 'child-process-gone', value: details.reason, frames: [] })
  })
}

/**
 * A main-process crash ends the app before it can report: Crashpad writes a minidump (kept on
 * this computer, never uploaded) and the next launch reports that it happened, then deletes the
 * dumps. Only the instance holding the single-instance lock: a second launch would take the first's marker.
 */
export function registerCrashDumps(host: HostController) {
  crashReporter.start({ uploadToServer: false })
  if (!crashedLastTime()) return
  // Sent once the host answers; it drops the report when analytics are off.
  const later = () => {
    if (host.state.kind !== 'running') return
    host.off('change', later)
    host.call('reportError', NATIVE_CRASH).catch(() => {})
  }
  host.on('change', later)
}

function crashedLastTime(): boolean {
  const marker = path.join(app.getPath('userData'), 'running')
  const dir = app.getPath('crashDumps')
  const mtime = (f: string) => {
    try {
      return fs.statSync(f).mtimeMs
    } catch {
      return null
    }
  }
  let dumps: string[] = []
  try {
    dumps = fs.readdirSync(dir, { recursive: true, encoding: 'utf8' }).filter((f) => f.endsWith('.dmp')).map((f) => path.join(dir, f))
  } catch {
    // No dump folder yet.
  }
  const crashed = mainCrashed(mtime(marker), dumps.map(mtime).filter((t) => t !== null))
  for (const f of dumps) fs.rmSync(f, { force: true })
  try {
    fs.writeFileSync(marker, '')
    app.on('will-quit', () => fs.rmSync(marker, { force: true }))
  } catch {
    // Without the marker a crash goes unreported; the app runs as before.
  }
  return crashed
}
