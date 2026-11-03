// The kith command line the Mac app needs, and the decision to install the copy it bundles.
// The host refuses agent runs on a CLI below its MIN_CLI (serve/host/src/kernel.rs); the app
// declares the same floor in package.json ("kith": { "cli": ... }) and installs its bundled CLI
// (Resources/kith, via Resources/kith-install.sh) at launch when the installed one is missing or older.

import { execFile } from 'node:child_process'
import { join } from 'node:path'

export type Triple = [number, number, number]

/** `1.2.3`, `v1.2.3` or `1.2.3-rc.1` as numbers (a pre-release suffix is ignored, like the host's); null if not a version. */
export function parseVersion(text: string | null | undefined): Triple | null {
  if (!text) return null
  const parts = text.trim().replace(/^v/, '').split(/[-+]/)[0].split('.')
  if (parts.length !== 3 || !parts.every((p) => /^\d+$/.test(p))) return null
  return parts.map(Number) as Triple
}

/** Negative, zero or positive like a comparator; an unparseable side counts as older than any version. */
export function compareVersions(a: string | null | undefined, b: string | null | undefined): number {
  const x = parseVersion(a)
  const y = parseVersion(b)
  if (!x || !y) return x ? 1 : y ? -1 : 0
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] - y[i]
  return 0
}

export interface MachineDirInput {
  /** KITH_STATE, else HQ_STATE. */
  state?: string | undefined
  home: string
  exists(path: string): boolean
  /** A real folder (not a symlink) at this path. */
  isRealDir(path: string): boolean
}

/** The machine folder, like serve/host/src/kernel.rs machine_dir: ~/.kith once migrated, else a real ~/.hq, else ~/.kith. */
export function machineDir({ state, home, exists, isRealDir }: MachineDirInput): string {
  if (state) return state
  const next = join(home, '.kith')
  const old = join(home, '.hq')
  if (exists(join(next, '.migrated'))) return next
  return isRealDir(old) ? old : next
}

/** Where the installed kernel's VERSION may be: the versioned layout's `current`, then the old flat install. */
export const installedVersionFiles = (machine: string) => [
  join(machine, 'cli/current/share/kith/kernel/VERSION'),
  join(machine, 'share/kith/kernel/VERSION'),
]

/** The first version file that holds a version (a flat install with no VERSION is "none": too old). */
export function installedVersion(machine: string, read: (path: string) => string | null): string | null {
  for (const file of installedVersionFiles(machine)) {
    const text = read(file)?.trim()
    if (text && parseVersion(text)) return text
  }
  return null
}

/** A launchd plist that sets KITH_KERNEL / HQ_KERNEL to something: a developer's kernel, never replaced. */
export function plistSetsKernel(plist: string | null): boolean {
  if (!plist) return false
  return /<key>(KITH|HQ)_KERNEL<\/key>\s*<string>[^<]+<\/string>/.test(plist)
}

export type CliDecision =
  | { action: 'install' }
  | { action: 'skip'; why: 'dev' | 'satisfied' | 'noBundle' | 'bundleNotNewer' }

export interface CliFacts {
  /** KITH_KERNEL is set for the app or its host service. */
  dev: boolean
  required: string
  installed: string | null
  /** The version of the CLI shipped in the app (null: this build has none). */
  bundled: string | null
}

/** Install the bundled CLI only when the installed one is missing or below the requirement and the bundle is newer: never a downgrade. */
export function decide({ dev, required, installed, bundled }: CliFacts): CliDecision {
  if (dev) return { action: 'skip', why: 'dev' }
  if (installed && compareVersions(installed, required) >= 0) return { action: 'skip', why: 'satisfied' }
  if (!bundled) return { action: 'skip', why: 'noBundle' }
  if (installed && compareVersions(bundled, installed) <= 0) return { action: 'skip', why: 'bundleNotNewer' }
  return { action: 'install' }
}

export interface InstallResult {
  ok: boolean
  /** One line for the user when it failed. */
  message: string
  output: string
}

export interface InstallerPaths {
  script: string
  /** The unpacked release folder (bin/kith, share/kith/kernel). */
  dir: string
}

/** Runs the installer unattended (scripts/install.sh's CLI-only mode); its failure is one line on stderr. */
export function runInstaller({ script, dir }: InstallerPaths, env: NodeJS.ProcessEnv, timeoutMs = 180_000): Promise<InstallResult> {
  return new Promise((resolve) => {
    execFile(
      'sh',
      [script],
      { env: { ...env, KITH_INSTALL_CLI_ONLY: '1', KITH_INSTALL_DIR: dir, KITH_INSTALL_YES: '1' }, timeout: timeoutMs, maxBuffer: 4 * 1024 * 1024, windowsHide: true },
      (error, stdout, stderr) => {
        const output = `${stdout}${stderr}`.trim()
        if (!error) return resolve({ ok: true, message: '', output })
        const lastLine = stderr.trim().split('\n').pop()?.trim()
        const message = error.killed
          ? `The kith CLI install didn't finish within ${Math.round(timeoutMs / 1000)} seconds.`
          : lastLine || error.message
        resolve({ ok: false, message, output })
      },
    )
  })
}

/** What the host's `hello.cli` says; the notice shows when it can't run agents. */
export interface HelloCli {
  version?: string | null
  min?: string
  ok?: boolean
  dev?: boolean
}

/** The notice for a CLI the host refuses (null: fine, or the host didn't say). */
export function cliNoticeFromHello(cli: HelloCli | null | undefined): string | null {
  if (!cli || cli.ok !== false) return null
  const have = cli.version ? `The kith CLI ${cli.version} is too old` : 'The kith CLI is not installed'
  return `${have}${cli.min ? `; this app needs ${cli.min} or newer` : ''}. Agents can't run until it is.`
}
