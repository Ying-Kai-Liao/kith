import type { SSHProfile } from '../shared/ssh.ts'

// Preserve the login shell's PATH, then try the supported installers, the Mac app bundle and the
// binary `kith serve start` installs in the data folder.
const REMOTE_PATH = '$PATH:/opt/homebrew/bin:/usr/local/bin:/home/linuxbrew/.linuxbrew/bin:$HOME/.local/bin:/Applications/Kith.app/Contents/Resources:$HOME/.kith/serve/bin'

/** The caller validates the port before constructing the remote shell command. */
export const remoteInfoCommand = (port: number) =>
  `sh -lc 'export KITH_SERVE_HOME="$HOME/.kith/serve"; PATH="${REMOTE_PATH}" kith-serve info --json --port ${port}'`

/** `dir` is this app's host data folder (`~/.kith/serve`, or a development sandbox's). */
export const knownHostsFiles = (h: string, dir = `${h}/.kith/serve`) => [`${h}/.ssh/known_hosts`, `${dir}/ssh_known_hosts`]

/** `pkill -f` pattern for the tunnels that use `dir`'s known_hosts file: this copy's own, never another copy's. */
export const orphanTunnelPattern = (dir: string) => `^/usr/bin/ssh -N .*${dir.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}/ssh_known_hosts`

/** ssh's config tokenizer splits `UserKnownHostsFile` on spaces, so each path carries literal quotes. */
export const hostKeyOptions = (h: string, dir?: string) => [
  '-o', 'UserKnownHostsFile=' + knownHostsFiles(h, dir).map((f) => `"${f}"`).join(' '),
  '-o', 'StrictHostKeyChecking=yes',
]

function targetOptions(p: SSHProfile) {
  const args: string[] = []
  if (p.port !== null) args.push('-p', String(p.port))
  if (p.identityFile !== null) args.push('-i', p.identityFile)
  if (p.user !== null) args.push('-l', p.user)
  return args
}

export function resolveArguments(p: SSHProfile) {
  const args = ['-G']
  if (p.port !== null) args.push('-p', String(p.port))
  if (p.user !== null) args.push('-l', p.user)
  return [...args, '--', p.host]
}

/** The remote command is fixed; only the validated port number goes into it. */
export const infoArguments = (p: SSHProfile, h: string, dir = `${h}/.kith/serve`) => [
  '-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '-o', 'ForwardAgent=no', '-o', 'ForwardX11=no',
  ...hostKeyOptions(h, dir), ...targetOptions(p),
  '--', p.host, remoteInfoCommand(p.remotePort),
]

export const tunnelArguments = (p: SSHProfile, localPort: number, h: string, dir?: string) => [
  '-N', '-T', '-o', 'ExitOnForwardFailure=yes', '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=3',
  // A shared master owns the listener and lets this child exit. The app must own
  // its tunnel process to verify the listener and close the forward on disconnect.
  '-S', 'none',
  '-o', 'ForwardAgent=no', '-o', 'ForwardX11=no', '-o', 'BatchMode=yes',
  ...hostKeyOptions(h, dir),
  '-L', `127.0.0.1:${localPort}:127.0.0.1:${p.remotePort}`,
  ...targetOptions(p), '--', p.host,
]
