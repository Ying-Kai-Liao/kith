// Runs the built web client (npm run build:web output, built fresh into a temp folder here) in a real Chromium at phone
// width, served with the strict headers the hosting package will set (design §4.3), and checks:
//  - the join screen and the too-old-browser screen render at 375px with no horizontal scroll (screenshots saved),
//  - the invite leaves the address bar and is not sitting in IndexedDB in the clear,
//  - no Trusted Types or CSP violations, and no script loaded from anywhere but the page's own origin (Clerk included),
//  - a browser without Ed25519 makes no request to the cloud or Clerk at all.
// `npm run test:web`. Needs Playwright's Chromium once: npx playwright-core install chromium
// Screenshots go to $KITH_WEB_SHOTS (default: a temp folder) and the paths are printed.
import { createServer } from 'node:http'
import { mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { extname, join, resolve } from 'node:path'
import { chromium } from 'playwright-core'
import { build } from 'vite'

const root = resolve(import.meta.dirname, '..')
const out = mkdtempSync(join(tmpdir(), 'kith-web-e2e-'))
const shots = process.env.KITH_WEB_SHOTS ?? mkdtempSync(join(tmpdir(), 'kith-web-shots-'))
mkdirSync(shots, { recursive: true })
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.woff2': 'font/woff2', '.png': 'image/png', '.svg': 'image/svg+xml' }

const CLOUD = 'https://kith-cloud-dev.lykevin890919.workers.dev'
const CLERK = 'https://adjusted-starfish-4735.clerk.accounts.dev'
// Exactly what the page needs to connect to (reported in the PR): the cloud over https and wss, and Clerk's Frontend API.
const CSP = [
  "default-src 'none'",
  "script-src 'self'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' blob: data:",
  `connect-src ${CLOUD} ${CLOUD.replace('https:', 'wss:')} ${CLERK}`,
  "font-src 'self'",
  "manifest-src 'self'",
  "base-uri 'none'",
  "form-action 'none'",
  "frame-ancestors 'none'",
  "object-src 'none'",
  "require-trusted-types-for 'script'",
].join('; ')

const failures = []
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`)
  if (!ok) failures.push(what)
}

let server
let browser
try {
  process.chdir(root) // the config resolves its paths from the working directory
  await build({ configFile: resolve(root, 'vite.web.config.ts'), logLevel: 'warn', build: { outDir: out, emptyOutDir: true } })
  server = createServer((req, res) => {
    const path = new URL(req.url, 'http://x').pathname
    let file = join(out, path.replace(/\.\.+/g, ''))
    let body
    try {
      body = readFileSync(file)
    } catch {
      // The single-page fallback the Worker will have: /join and friends are the app.
      file = join(out, 'index.html')
      body = readFileSync(file)
    }
    res.writeHead(200, {
      'content-type': TYPES[extname(file)] ?? 'application/octet-stream',
      'content-security-policy': CSP,
      'x-content-type-options': 'nosniff',
      'referrer-policy': 'no-referrer',
    })
    res.end(body)
  })
  await new Promise((r) => server.listen(0, '127.0.0.1', r))
  const base = `http://127.0.0.1:${server.address().port}`
  browser = await chromium.launch()
  console.log(`browser: Chromium ${browser.version()}`)

  const TOKEN = 'SECRETTOKEN0123456789abcdef'
  const link = `${base}/join#c=cmp_demo&i=inv_demo&t=${TOKEN}`

  async function visit(name, { noEd25519 = false } = {}) {
    const context = await browser.newContext({ viewport: { width: 375, height: 740 }, deviceScaleFactor: 2 })
    const page = await context.newPage()
    const violations = []
    const requests = []
    page.on('console', (m) => {
      const t = m.text()
      if (/Trusted ?Type|TrustedHTML|TrustedScript|Content Security Policy|violates the following/i.test(t)) violations.push(t)
    })
    page.on('pageerror', (e) => violations.push(`pageerror: ${e.message}`))
    page.on('request', (r) => requests.push({ url: r.url(), type: r.resourceType() }))
    if (noEd25519) {
      // A browser that lists WebCrypto but cannot do Ed25519: generateKey for it throws NotSupportedError.
      await page.addInitScript(() => {
        const original = SubtleCrypto.prototype.generateKey
        SubtleCrypto.prototype.generateKey = function (algorithm, ...rest) {
          if ((algorithm?.name ?? algorithm) === 'Ed25519') return Promise.reject(new DOMException('Unrecognized name.', 'NotSupportedError'))
          return original.call(this, algorithm, ...rest)
        }
      })
    }
    await page.goto(link)
    return { context, page, violations, requests, name }
  }

  // ---- 1. the join screen (a modern browser, signed out, with an invite link) ----
  {
    const v = await visit('join')
    await v.page.waitForSelector('[data-screen="sign-in"]', { timeout: 30_000 })
    await v.page.waitForTimeout(1500)
    const shot = join(shots, 'web-join-375.png')
    await v.page.screenshot({ path: shot })
    console.log(`screenshot: ${shot}`)
    check(!(await v.page.url()).includes('#') && !(await v.page.url()).includes(TOKEN), 'the invite fragment is gone from the address bar')
    const scroll = await v.page.evaluate(() => ({ sw: document.documentElement.scrollWidth, iw: window.innerWidth }))
    check(scroll.sw <= scroll.iw, `no horizontal scroll at 375px (scrollWidth ${scroll.sw}, width ${scroll.iw})`)
    const text = await v.page.innerText('body')
    check(/invited/i.test(text) && /Google/.test(text) && /GitHub/.test(text), 'the join screen offers Google and GitHub for an invited person')
    const cloudRequests = v.requests.filter((r) => r.url.startsWith(CLOUD))
    check(cloudRequests.length === 0, 'signed out, the page makes no request to the cloud')
    const foreignScripts = v.requests.filter((r) => r.type === 'script' && !r.url.startsWith(base))
    check(foreignScripts.length === 0, `no script loaded from another origin (${foreignScripts.map((r) => r.url).join(', ') || 'none'})`)
    const origins = [...new Set(v.requests.map((r) => new URL(r.url).origin))]
    console.log(`origins the page contacted: ${origins.join(', ')}`)
    check(v.violations.length === 0, `no Trusted Types or CSP violations (${v.violations.join(' | ') || 'none'})`)
    // The token is sealed before sign-in: look through every IndexedDB record for it in the clear.
    const dump = await v.page.evaluate(async () => {
      const db = await new Promise((res, rej) => {
        const r = indexedDB.open('kith-web', 1)
        r.onsuccess = () => res(r.result)
        r.onerror = () => rej(r.error)
      })
      const rows = {}
      for (const name of db.objectStoreNames) {
        rows[name] = await new Promise((res) => {
          const out = []
          const tx = db.transaction(name).objectStore(name).openCursor()
          tx.onsuccess = () => {
            const c = tx.result
            if (!c) return res(out)
            const val = c.value
            out.push({ key: c.key, bytes: val?.data ? Array.from(new Uint8Array(val.data)) : null, extractable: val?.privateKey?.extractable ?? null })
            c.continue()
          }
        })
      }
      return rows
    })
    const sealed = dump.invites?.find((r) => r.key === 'pending')
    check(!!sealed && sealed.bytes.length > 0, 'the waiting invite is stored in IndexedDB')
    check(sealed && !Buffer.from(sealed.bytes).includes(Buffer.from(TOKEN)), 'the invite token is not in IndexedDB in the clear')
    await v.context.close()
  }

  // ---- 2. a browser without Ed25519: the too-old screen, and nothing sent anywhere ----
  {
    const v = await visit('too-old', { noEd25519: true })
    await v.page.waitForSelector('[data-screen="too-old"]', { timeout: 30_000 })
    const shot = join(shots, 'web-too-old-375.png')
    await v.page.screenshot({ path: shot })
    console.log(`screenshot: ${shot}`)
    const text = await v.page.innerText('body')
    check(/too old for Kith/i.test(text) && /Chrome, Safari or Firefox/.test(text), 'the too-old screen says what to do')
    const scroll = await v.page.evaluate(() => ({ sw: document.documentElement.scrollWidth, iw: window.innerWidth }))
    check(scroll.sw <= scroll.iw, `no horizontal scroll at 375px (scrollWidth ${scroll.sw}, width ${scroll.iw})`)
    const external = v.requests.filter((r) => !r.url.startsWith(base) && !r.url.startsWith('data:') && !r.url.startsWith('blob:'))
    check(external.length === 0, `a browser that fails the check contacts no one (${external.map((r) => r.url).join(', ') || 'none'})`)
    check(!(await v.page.url()).includes(TOKEN), 'and the invite is not kept in the address bar')
    const dump = await v.page.evaluate(async () => (await indexedDB.databases?.())?.map((d) => d.name) ?? [])
    check(!dump.includes('kith-web'), 'no IndexedDB database, so no key, is made')
    check(v.violations.length === 0, `no Trusted Types or CSP violations (${v.violations.join(' | ') || 'none'})`)
    await v.context.close()
  }

  // ---- 3. a laptop width still lays out ----
  {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    await page.goto(`${base}/`)
    await page.waitForSelector('[data-screen="sign-in"]', { timeout: 30_000 })
    const shot = join(shots, 'web-signin-1280.png')
    await page.screenshot({ path: shot })
    console.log(`screenshot: ${shot}`)
    await context.close()
  }
} finally {
  await browser?.close()
  server?.close()
  rmSync(out, { recursive: true, force: true })
}
console.log(failures.length ? `${failures.length} check(s) failed` : 'all checks passed')
process.exit(failures.length ? 1 : 0)
