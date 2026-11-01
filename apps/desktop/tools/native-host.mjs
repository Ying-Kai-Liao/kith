// Builds the host the app bundles: node tools/native-host.mjs. macOS: build/native/kith-serve,
// build/native/kith-tts (arm64) and build/native/kith/ (bin/kith + share/kith/kernel, the release
// layout; universal). `fast` (the sandbox runs of tools/dev.mjs): the host's `dev-app` profile for this
// Mac's architecture only, since that build never leaves the Mac that made it. KITH_HOST_NATIVE_ONLY=1
// does the same for the release profile (CI's unsigned build: its runner has no x86_64 target). Windows:
// build/native/kith-serve.exe and kith-servew.exe (runs it at sign-in), plus the computer-use
// driver the host starts from beside itself. Linux apps use the installed host, so there's
// nothing to do there.
import { copyFileSync, mkdirSync, rmSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

if (!['darwin', 'win32'].includes(process.platform)) process.exit(0)
const fast = process.argv[2] === 'fast'
const nativeOnly = fast || process.env.KITH_HOST_NATIVE_ONLY === '1'
const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const host = join(root, '../../serve/host')
const out = join(root, 'build/native')
// The pinned toolchain (rust-toolchain.toml) through rustup, even when another cargo (Homebrew's)
// comes first on PATH: only rustup's has the x86_64 target.
const tool = (name) => execFileSync('rustup', ['which', name], { cwd: host, encoding: 'utf8' }).trim()
const [cargo, rustc] = [tool('cargo'), tool('rustc')]
const run = (cmd, args) => execFileSync(cmd, args, { stdio: 'inherit', env: { ...process.env, RUSTC: rustc } })
// Seconds to rebuild instead of minutes.
const profile = fast ? 'dev-app' : 'release'
const build = (target, ...more) => run(cargo, ['build', '--profile', profile, '--manifest-path', join(host, 'Cargo.toml'), '--target', target, ...more])
// Cargo names a profile's folder after it, except dev/release.
const built = (target, name) => join(process.env.CARGO_TARGET_DIR ?? join(host, 'target'), target, profile, name)
mkdirSync(out, { recursive: true })
if (process.platform === 'win32') {
  const target = 'x86_64-pc-windows-msvc'
  build(target)
  copyFileSync(built(target, 'kith-serve.exe'), join(out, 'kith-serve.exe'))
  copyFileSync(built(target, 'kith-servew.exe'), join(out, 'kith-servew.exe'))
  run(process.execPath, [join(root, '../packaging/cua-driver/fetch.mjs'), 'windows-x86_64', out])
} else {
  const native = process.arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin'
  const targets = nativeOnly ? [native] : ['aarch64-apple-darwin', 'x86_64-apple-darwin']
  for (const target of targets) build(target)
  // The local voice helper (kith-tts, its own crate) ships beside kith-serve; Apple silicon only, so an Intel
  // build has none and the host reports local voice unavailable.
  const arm = 'aarch64-apple-darwin'
  rmSync(join(out, 'kith-tts'), { force: true })
  if (targets.includes(arm)) {
    build(arm, '-p', 'kith-tts')
    copyFileSync(built(arm, 'kith-tts'), join(out, 'kith-tts'))
  }
  run('lipo', ['-create', '-output', join(out, 'kith-serve'), ...targets.map((t) => built(t, 'kith-serve'))])
  // The same crate's `kith` name, laid out like the release tarball (bin/kith, share/kith/kernel...) so the
  // bundled kith finds its kernel the way an installed one does. The service keeps running kith-serve.
  const kith = join(out, 'kith')
  rmSync(kith, { recursive: true, force: true })
  mkdirSync(join(kith, 'bin'), { recursive: true })
  run('lipo', ['-create', '-output', join(kith, 'bin/kith'), ...targets.map((t) => built(t, 'kith'))])
  run('sh', [join(root, '../../scripts/stage-kernel.sh'), join(kith, 'share/kith')])
}
console.log('host: built')
