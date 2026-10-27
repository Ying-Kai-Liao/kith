// Fail before packaging if a stale account-config.json belongs to the other app variant.
const { accessSync, readFileSync } = require('node:fs')
const { join } = require('node:path')
module.exports = async (context) => {
  const expected = context.packager.appInfo.id === 'com.yingkailiao.kith.dev' ? 'dev' : 'main'
  const config = JSON.parse(readFileSync(join(context.packager.projectDir, 'resources/account-config.json'), 'utf8'))
  if (config.environment !== expected) throw new Error(`Packaging ${expected} with ${config.environment} account config; run the matching dist script.`)
  if (context.electronPlatformName === 'darwin') {
    const symbols = join(context.packager.projectDir, 'out/renderer/symbols')
    const manifest = JSON.parse(readFileSync(join(symbols, 'manifest.json'), 'utf8'))
    if (!manifest['apple.logo']) throw new Error('The macOS build is missing the Apple logo; rebuild the app on macOS.')
    for (const weight of ['light', 'regular', 'medium', 'semibold', 'bold', 'black']) {
      accessSync(join(symbols, `apple.logo.${weight}.png`))
    }
    // electron-builder skips an extraResources/extraFiles source that doesn't exist, so a missing helper would
    // ship a broken app without a word. kith-tts is Apple silicon only: an Intel Dev build has none.
    const { mac = {}, dmg = {} } = context.packager.config ?? {}
    const sources = [...(context.packager.config?.extraResources ?? []), ...(mac.extraResources ?? []), ...(mac.extraFiles ?? [])]
      .map((item) => (typeof item === 'string' ? item : item.from))
    if (dmg.background && expected === 'main') sources.push(dmg.background)
    for (const from of sources) {
      if (from.endsWith('/kith-tts') && expected === 'dev' && process.arch !== 'arm64') continue
      try {
        accessSync(join(context.packager.projectDir, from))
      } catch {
        throw new Error(`The macOS build is missing ${from}; run the native build steps (npm run dist:mac:${expected} does).`)
      }
    }
  }
}
