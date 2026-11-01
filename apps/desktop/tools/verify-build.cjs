const { accessSync, readFileSync } = require('node:fs')
const { join } = require('node:path')
module.exports = async (context) => {
  if (context.electronPlatformName === 'darwin') {
    const symbols = join(context.packager.projectDir, 'out/renderer/symbols')
    const manifest = JSON.parse(readFileSync(join(symbols, 'manifest.json'), 'utf8'))
    if (!manifest['apple.logo']) throw new Error('The macOS build is missing the Apple logo; rebuild the app on macOS.')
    for (const weight of ['light', 'regular', 'medium', 'semibold', 'bold', 'black']) {
      accessSync(join(symbols, `apple.logo.${weight}.png`))
    }
    // electron-builder skips an extraResources/extraFiles source that doesn't exist, so a missing helper would
    // ship a broken app without a word.
    const { mac = {}, dmg = {} } = context.packager.config ?? {}
    const sources = [...(context.packager.config?.extraResources ?? []), ...(mac.extraResources ?? []), ...(mac.extraFiles ?? [])]
      .map((item) => (typeof item === 'string' ? item : item.from))
    if (dmg.background) sources.push(dmg.background)
    for (const from of sources) {
            try {
        accessSync(join(context.packager.projectDir, from))
      } catch {
        throw new Error(`The macOS build is missing ${from}; run the native build steps (npm run dist:mac does).`)
      }
    }
  }
}
