// Builds the Feed's page-picture helper (serve/packaging/shoot, a SwiftPM package) into build/native/kith-shoot for
// the Mac app to bundle next to the host: node tools/native-shoot.mjs. Universal (arm64 + x86_64).
import { execFileSync } from 'node:child_process'
import { copyFileSync, mkdirSync, rmSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

if (process.platform !== 'darwin') process.exit(0)
const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const pkg = join(root, '../../serve/packaging/shoot')
const out = join(root, 'build/native')
const archs = ['arm64', 'x86_64']
mkdirSync(out, { recursive: true })
rmSync(join(out, 'kith-shoot'), { force: true })
const binaries = archs.map((arch) => {
  const scratch = join(root, `build/shoot-${arch}`)
  execFileSync('swift', ['build', '-c', 'release', '--package-path', pkg, '--scratch-path', scratch, '--triple', `${arch}-apple-macosx`], { stdio: 'inherit' })
  return join(scratch, `${arch}-apple-macosx/release/kith-shoot`)
})
if (binaries.length === 1) copyFileSync(binaries[0], join(out, 'kith-shoot'))
else execFileSync('lipo', ['-create', '-output', join(out, 'kith-shoot'), ...binaries], { stdio: 'inherit' })
console.log('shoot: built')
