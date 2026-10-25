// The web client's join confirmation, run in real Chromium at 375px: a signed-in person with an invite waiting sees
// "Join a computer on Kith?" and NOTHING runs (no takePending, accept, requestAccess or proveInvite) until Join is clicked;
// "Not now" discards the invite and runs nothing either. The real WebWindow runs on the demo's fake host.
// `npm run test:web` (after web-e2e). Screenshots go to $KITH_WEB_SHOTS (default: a temp folder) and the paths are printed.
import { createServer } from 'node:http'
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { extname, join, resolve } from 'node:path'
import { chromium } from 'playwright-core'
import { build } from 'vite'
import react from '@vitejs/plugin-react'

const root = resolve(import.meta.dirname, '..')
const work = mkdtempSync(join(realpathSync(tmpdir()), 'kith-web-confirm-'))
const shots = process.env.KITH_WEB_SHOTS ?? mkdtempSync(join(tmpdir(), 'kith-web-shots-'))
mkdirSync(shots, { recursive: true })
const src = join(root, 'src/renderer')

writeFileSync(join(work, 'index.html'), '<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"></head><body><div id="root"></div><script type="module" src="./entry.ts"></script></body></html>')
writeFileSync(
  join(work, 'fake.ts'),
  `import ${JSON.stringify(src + '/demo/bridge')}
import { FakeHost } from ${JSON.stringify(src + '/demo/fake-host')}
import { provideWebSession } from ${JSON.stringify(src + '/web/session')}
const host = new FakeHost('0.0.0')
const k = window.kith as any
const calls: string[] = (window as any).__calls = []
const note = (name: string) => (...a: unknown[]) => { calls.push(name); return a }
k.platform = 'web'
k.account.state = async () => ({ ready: true, configured: true, busy: false, error: null, user: { userId: 'u1', email: 'brother@example.com', avatarURL: null }, cloudURL: 'https://x' })
k.cloud.request = async () => ({ ok: true, value: {} })
k.cloud.acceptInvite = async () => { note('acceptInvite')(); return { ok: true, value: { computerId: 'demo-mac' } } }
k.invites.takePending = async () => { note('takePending')(); return { computerId: 'demo-mac', inviteId: 'inv_1', token: 'tok_12345678' } }
k.relay.computers = async () => ({ ok: true, value: [] })
k.relay.requestAccess = async () => { note('requestAccess')(); return { ok: false, error: { status: 0, code: 'x', message: 'x' } } }
k.relay.call = async (_c: string, _k: string, method: string, body: unknown) => { if (/proveInvite/.test(method)) note('proveInvite')(); return host.call(method, (body ?? {}) as Record<string, unknown>) }
k.relay.state = async () => ({ kind: 'ready' })
let pending: { computerId: string } | null = { computerId: 'demo-mac-computer-id' }
provideWebSession({
  ready: async () => {},
  support: async () => null,
  pendingLink: async () => pending && { ...pending, appLink: 'kith-dev://invite?c=demo-mac-computer-id&i=inv_1&t=tok_12345678' },
  discardPending: async () => { calls.push('discardPending'); pending = null },
  signInAgain: async () => {},
})
`,
)

writeFileSync(join(work, 'entry.ts'), `import './fake'\nimport ${JSON.stringify(src + '/main')}\n`)

const out = join(work, 'dist')
await build({
  root: work,
  configFile: false,
  logLevel: 'error',
  plugins: [react()],
  resolve: { alias: { '@shared': join(root, 'src/shared') } },
  server: { fs: { strict: false } },
  build: { outDir: out, emptyOutDir: true, chunkSizeWarningLimit: 9000 },
})
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.png': 'image/png', '.svg': 'image/svg+xml' }
const server = createServer((req, res) => {
  let f = join(out, new URL(req.url, 'http://x').pathname)
  let body
  try {
    body = readFileSync(f)
  } catch {
    f = join(out, 'index.html')
    body = readFileSync(f)
  }
  res.writeHead(200, { 'content-type': TYPES[extname(f)] ?? 'application/octet-stream' })
  res.end(body)
})
await new Promise((r) => server.listen(0, '127.0.0.1', r))
const base = `http://127.0.0.1:${server.address().port}/`

const failures = []
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`)
  if (!ok) failures.push(what)
}

const browser = await chromium.launch()
try {
  async function open() {
    const page = await (await browser.newContext({ viewport: { width: 375, height: 740 }, deviceScaleFactor: 2 })).newPage()
    await page.goto(base)
    await page.waitForSelector('[data-screen="confirm-join"]', { timeout: 15000 })
    await page.waitForTimeout(800)
    return page
  }
  const calls = (page) => page.evaluate(() => window.__calls)

  const waiting = await open()
  const shot = join(shots, 'join-confirm-375.png')
  await waiting.screenshot({ path: shot })
  console.log(`screenshot: ${shot}`)
  const [sw, iw] = await waiting.evaluate(() => [document.documentElement.scrollWidth, window.innerWidth])
  check(sw <= iw, `the confirm screen has no horizontal scroll at 375px (${sw}/${iw})`)
  const text = await waiting.innerText('body')
  check(/Join a computer on Kith\?/.test(text) && /Not now/.test(text), 'the confirm screen asks before joining')
  check((await calls(waiting)).length === 0, 'nothing ran before the click: no takePending, accept, requestAccess or proveInvite')
  const openApp = waiting.getByRole('link', { name: 'Open in the Kith app' })
  check((await openApp.getAttribute('href')) === 'kith-dev://invite?c=demo-mac-computer-id&i=inv_1&t=tok_12345678', 'the app offer is a plain anchor to the kith-dev link')
  await waiting.waitForTimeout(1500)
  check(waiting.url() === base, 'no auto-redirect')
  check((await calls(waiting)).length === 0, 'and still nothing after waiting')
  await waiting.getByRole('button', { name: 'Join', exact: true }).click()
  await waiting.waitForTimeout(1500)
  const after = await calls(waiting)
  check(after.includes('takePending'), `Join starts the join (${after.join(', ') || 'no calls'})`)
  await waiting.context().close()

  const declined = await open()
  await declined.getByRole('button', { name: 'Not now' }).click()
  await declined.waitForTimeout(1000)
  const dc = await calls(declined)
  check(dc.includes('discardPending'), 'Not now discards the waiting invite')
  check(!dc.some((c) => ['takePending', 'acceptInvite', 'requestAccess', 'proveInvite'].includes(c)), 'and runs nothing')
  await declined.context().close()
} finally {
  await browser.close()
  server.close()
}
if (failures.length) {
  console.error(`\n${failures.length} check(s) failed`)
  process.exit(1)
}
console.log('\nall checks passed')
