// Builds the on-device speech helper (native/speech-macos, a SwiftPM package) into build/native/kith-speech
// for the Mac app to bundle: node tools/native-speech.mjs. Universal (arm64 + x86_64).
import { execFileSync } from 'node:child_process'
import { copyFileSync, mkdirSync, rmSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

if (process.platform !== 'darwin') process.exit(0)
const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const pkg = join(root, 'native/speech-macos')
const out = join(root, 'build/native')
const archs = ['arm64', 'x86_64']
mkdirSync(out, { recursive: true })
rmSync(join(out, 'kith-speech'), { force: true })
const binaries = archs.map((arch) => {
  const scratch = join(root, `build/speech-${arch}`)
  execFileSync('swift', ['build', '-c', 'release', '--package-path', pkg, '--scratch-path', scratch, '--triple', `${arch}-apple-macosx`], { stdio: 'inherit' })
  return join(scratch, `${arch}-apple-macosx/release/kith-speech`)
})
if (binaries.length === 1) copyFileSync(binaries[0], join(out, 'kith-speech'))
else execFileSync('lipo', ['-create', '-output', join(out, 'kith-speech'), ...binaries], { stdio: 'inherit' })
console.log('speech: built')
