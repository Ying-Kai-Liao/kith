import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { app } from 'electron'
import { sandboxLabel } from '../shared/dev-sandbox'
import { identity } from '../shared/environment'

interface BuildConfig {
  clerkPublishableKey: string | null
  cloudURL: string | null
}

export { identity }

const path = app.isPackaged
  ? join(process.resourcesPath, 'account-config.json')
  : join(__dirname, '../../resources/account-config.json')
export const buildConfig = JSON.parse(readFileSync(path, 'utf8')) as BuildConfig

// Before the single-instance lock, preferences, account restore or Chromium session is created.
// A development run against its own host data (`KITH_SERVE_HOME`, a worktree's sandbox) keeps all of
// that there too, under its own name, so it runs beside the main checkout's Kith.
const sandbox = app.isPackaged ? undefined : process.env.KITH_SERVE_HOME
if (sandbox) {
  app.setName(sandboxLabel(identity.name, sandbox))
  app.setPath('userData', join(sandbox, 'desktop'))
}
