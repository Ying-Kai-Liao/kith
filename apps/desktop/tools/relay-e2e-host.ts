// The desktop relay client against a REAL kith-serve: `wrangler dev --env local` (serve/cloud) plus the host binary in a
// temporary KITH_SERVE_HOME / KITH_AUTH, claimed by a self-signed Clerk test user (as serve/cloud/test/e2e/account.e2e.ts).
// A second device (the desktop's key) registers, asks for access, the host shows the same six digits and approves, and the
// desktop's RelayLink connects over the relay, handshakes with the real host and calls `hello` and `people`, and opens the
// event stream.
//
//   npm --prefix serve/cloud ci
//   (cd serve/host && cargo build --bin kith-serve)     debug only; or set KITH_SERVE_HOST_BIN
//   node --experimental-strip-types tools/relay-e2e-host.ts
//
// Heavy (workerd + the host): run it through the flow test_slot at background priority. No ~/.kith, no real kith serve.

import { type ChildProcess, spawn, spawnSync } from 'node:child_process'
import { generateKeyPairSync } from 'node:crypto'
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { requestAccess } from '../src/shared/relay-core/access.ts'
import { b64url, signatureHeader, signerFromSeed, unb64url, type Signer } from '../src/shared/relay-core/crypto.ts'
import { RelayLink } from '../src/shared/relay-core/link.ts'
import { dialWebSocket, probeUpgrade } from '../src/main/relay/dial.ts'
import { parseInviteLink } from '../src/shared/invite.ts'
import { runJoin, type JoinState } from '../src/shared/invite-join.ts'
import { relayJoinDeps, type RelayApi } from '../src/shared/relay-access.ts'
import { botGuests } from './relay-e2e-bot.ts'

const ROOT = fileURLToPath(new URL('../../../', import.meta.url))
const CLOUD_DIR = join(ROOT, 'serve/cloud')
const KERNEL_BIN = join(ROOT, 'kernel/bin')
const HOST_BIN = process.env.KITH_SERVE_HOST_BIN ?? join(resolve(join(ROOT, 'serve/host'), process.env.CARGO_TARGET_DIR ?? 'target'), 'debug/kith-serve')
const PORT = Number(process.env.KITH_E2E_RELAY_PORT ?? 8792)
const CLOUD = `http://127.0.0.1:${PORT}`
const ISSUER = 'https://clerk.test.local' // [env.local] CLERK_ISSUER
const WRANGLER = join(CLOUD_DIR, 'node_modules/.bin/wrangler')
const base = process.env.KITH_E2E_CACHE ?? '/Volumes/Macos 1TB/.cache'
const parent = existsSync(base) ? base : tmpdir()
mkdirSync(parent, { recursive: true })
const tmp = mkdtempSync(join(parent, 'kith-desktop-relay-host-e2e-'))
const persist = join(tmp, 'wrangler')
const home = join(tmp, 'home')
const authFile = join(tmp, 'auth.json')
const children: ChildProcess[] = []
// The last lines each child printed (wrangler, the host), shown when the run fails: CI has no E2E_VERBOSE, and without
// them a failure such as the relay's "Can't reach your computer" says nothing about what the host did.
const tails = new Map<string, string[]>()

function check(cond: unknown, what: string): asserts cond {
  if (!cond) throw new Error(`check failed: ${what}`)
  console.log(`  ok ${what}`)
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
async function waitFor<T>(what: string, f: () => T | undefined | false | Promise<T | undefined | false>, ms = 20_000): Promise<T> {
  const end = Date.now() + ms
  for (;;) {
    const v = await f()
    if (v) return v as T
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`)
    await sleep(150)
  }
}

function run(cmd: string, args: string[], env: Record<string, string> = {}): ChildProcess {
  const child = spawn(cmd, args, { cwd: CLOUD_DIR, env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] })
  const name = cmd.split('/').pop() ?? cmd
  const tail = tails.get(name) ?? []
  tails.set(name, tail)
  const keep = (d: unknown) => {
    tail.push(...String(d).split('\n').filter(Boolean))
    tail.splice(0, Math.max(0, tail.length - 400))
  }
  child.stdout?.on('data', (d) => (keep(d), process.env.E2E_VERBOSE && process.stdout.write(String(d))))
  child.stderr?.on('data', (d) => (keep(d), process.env.E2E_VERBOSE && process.stderr.write(String(d))))
  child.once('exit', (code, sig) => keep(`[${name} exited: ${sig ?? code}]`))
  children.push(child)
  return child
}

async function stop(child: ChildProcess) {
  if (child.exitCode !== null || child.signalCode) return
  const exited = new Promise((r) => child.once('exit', r))
  child.kill('SIGTERM')
  await Promise.race([exited, sleep(5000)])
  if (child.exitCode === null && !child.signalCode) child.kill('SIGKILL')
}

const freePort = () =>
  new Promise<number>((res) => {
    const s = createServer()
    s.listen(0, '127.0.0.1', () => {
      const port = (s.address() as { port: number }).port
      s.close(() => res(port))
    })
  })

const kithAuth = (...args: string[]) => spawnSync('python3', [join(KERNEL_BIN, 'kith-auth'), ...args], { encoding: 'utf8', env: { ...process.env, KITH_AUTH: authFile } })

const { publicKey, privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 })
const jwtKey = publicKey.export({ type: 'spki', format: 'pem' }).toString().replace(/-----[A-Z ]+-----|\s/g, '')
async function clerkToken(sub: string): Promise<string> {
  const key = await crypto.subtle.importKey('jwk', privateKey.export({ format: 'jwk' }), { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' }, false, ['sign'])
  const now = Math.floor(Date.now() / 1000)
  const part = (v: unknown) => b64url(Buffer.from(JSON.stringify(v)))
  const head = `${part({ alg: 'RS256', typ: 'JWT' })}.${part({ iss: ISSUER, sub, sid: `sess_${sub}`, iat: now - 5, nbf: now - 5, exp: now + 3600, email: `${sub}@example.com` })}`
  return `${head}.${b64url(new Uint8Array(await crypto.subtle.sign('RSASSA-PKCS1-v1_5', key, Buffer.from(head))))}`
}

async function cloud(method: string, path: string, o: { token?: string; signer?: Signer; body?: unknown } = {}) {
  const bytes = o.body === undefined ? Buffer.alloc(0) : Buffer.from(JSON.stringify(o.body))
  const headers: Record<string, string> = {}
  if (o.token) headers.Authorization = `Bearer ${o.token}`
  if (o.body !== undefined) headers['Content-Type'] = 'application/json'
  if (o.signer) headers['Kith-Sig'] = await signatureHeader(o.signer, { method, authority: `127.0.0.1:${PORT}`, pathAndQuery: path, body: bytes })
  const res = await fetch(CLOUD + path, { method, headers, body: bytes.length ? bytes : undefined })
  return { status: res.status, body: (await res.json()) as any }
}

let hostPort = 0
let hostChild: ChildProcess | undefined
async function loopback(method: string, body: unknown = {}): Promise<any> {
  const token = readFileSync(join(home, 'token'), 'utf8').trim()
  const res = await fetch(`http://127.0.0.1:${hostPort}/api/${method}`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  const v = await res.json()
  if (!res.ok) throw new Error(`${method} -> ${res.status} ${JSON.stringify(v)}`)
  return v
}

async function main() {
  if (!existsSync(WRANGLER)) throw new Error('wrangler is not installed: run `npm --prefix serve/cloud ci` first')
  if (spawnSync(HOST_BIN, ['--version']).error) throw new Error(`kith-serve not found at ${HOST_BIN}; build it or set KITH_SERVE_HOST_BIN`)
  const migrate = spawnSync(WRANGLER, ['d1', 'migrations', 'apply', 'kith-local', '--local', '--env', 'local', '--persist-to', persist], {
    cwd: CLOUD_DIR,
    encoding: 'utf8',
    env: { ...process.env, CI: '1' },
  })
  if (migrate.status !== 0) throw new Error(`migrations failed: ${migrate.stderr}`)
  run(WRANGLER, ['dev', '--env', 'local', '--ip', '127.0.0.1', '--port', String(PORT), '--persist-to', persist, '--var', `CLERK_JWT_KEY:${jwtKey}`])
  await waitFor('cloud /v1/health', () => fetch(`${CLOUD}/v1/health`).then((r) => r.ok).catch(() => false), 90_000)

  // The host's `kith` is a stand-in that lists one agent (`house`); kith-auth is the real one beside it.
  const kbin = join(tmp, 'kbin')
  mkdirSync(kbin, { recursive: true })
  copyFileSync(join(ROOT, 'serve/host/tests/fixtures/stub_hq.py'), join(kbin, 'kith'))
  chmodSync(join(kbin, 'kith'), 0o755)
  symlinkSync(join(KERNEL_BIN, 'kith-auth'), join(kbin, 'kith-auth'))
  mkdirSync(join(tmp, 'agent'), { recursive: true })
  // kith-auth only grants agents it can find in the Kith home.
  mkdirSync(join(tmp, 'kith-home/agents/house'), { recursive: true })
  writeFileSync(join(tmp, 'kith-home/agents/house/agent.md'), '# house\n')

  check(kithAuth('person', 'kai', 'owner').status === 0, 'owner person exists in the throwaway kith-auth')
  hostPort = await freePort()
  hostChild = run(HOST_BIN, ['serve', '--port', String(hostPort)], {
    KITH_SERVE_HOME: home,
    KITH_SERVE_CLOUD_URL: CLOUD,
    KITH_AUTH: authFile,
    KITH_KERNEL_BIN: join(kbin, 'kith'),
    STUB_HQ_HOME: join(tmp, 'agent'),
    KITH_STATE: join(tmp, 'state'),
    KITH_HOME: join(tmp, 'kith-home'),
    // The bot-guest stage asks this agent; it answers "Echo: <prompt>".
    KITH_SERVE_HQ_ACP_COMMAND: `python3 -X utf8 -u '${join(ROOT, 'serve/host/tests/fixtures/echo_agent.py')}' claude`,
  })
  await waitFor('host /health', () => fetch(`http://127.0.0.1:${hostPort}/health`).then((r) => r.ok).catch(() => false), 30_000)
  const computerId = ((await (await fetch(`http://127.0.0.1:${hostPort}/health`)).json()) as { computerId: string }).computerId
  check(Boolean(computerId), 'the real host has a computer id')

  const userId = 'user_desktop_e2e'
  const token = await clerkToken(userId)
  const claim = await cloud('POST', '/v1/claims', { token })
  const signed = await loopback('claimSign', { claimId: claim.body.claimId, nonce: claim.body.nonce, userId })
  check((await cloud('POST', `/v1/claims/${claim.body.claimId}/complete`, { token, body: signed })).status === 200, 'the test user claimed the real host')
  await waitFor('host sees its owner', async () => (await loopback('cloudStatus')).owner?.userId === userId)

  // The desktop's device key registers, then asks for access.
  const desktop = signerFromSeed(Buffer.alloc(32, 0x61)) // fake, low entropy
  const req = (method: string, path: string, body: unknown, signed: boolean) =>
    cloud(method, `/${path}`, { token, signer: signed ? desktop : undefined, body }).then((r) =>
      r.status >= 200 && r.status < 300 ? ({ ok: true, value: r.body } as const) : ({ ok: false, error: { status: r.status, code: r.body?.error?.code ?? `http${r.status}`, message: r.body?.error?.message ?? null } } as const),
    )
  check((await req('POST', 'v1/devices', { name: 'Desktop e2e', platform: 'macos' }, true)).ok, 'the desktop registered its device key')
  const listed = await req('GET', 'v1/computers', undefined, true)
  const row = listed.ok && (listed.value as any).computers.find((c: any) => c.computerId === computerId)
  check(row && row.access === 'none', 'the desktop lists the real host, no access yet')

  let requestId = ''
  const ticket = await Promise.all([
    requestAccess({ request: req, signer: desktop, computerId, signKey: row.signKey, cancelled: () => false, created: (id) => (requestId = id), pollMs: 300 }),
    (async () => {
      // The host shows the six digits once the desktop revealed its nonce.
      const shown = await waitFor('the six digits on the real host', async () => (await loopback('accessRequests')).requests.find((r: any) => r.code))
      return shown as { requestId: string; code: string }
    })(),
  ])
  check(ticket[0].ok, 'the desktop got a ticket')
  check(ticket[0].ok && ticket[0].value.code === ticket[1].code, `the real host shows the desktop's six digits (${ticket[1].code})`)
  await loopback('decideAccessRequest', { requestId: ticket[1].requestId, approve: true })
  await waitFor('approved', async () => (await req('GET', `v1/access-requests/${requestId}`, undefined, false)).ok && ((await req('GET', `v1/access-requests/${requestId}`, undefined, false)) as any).value.status === 'approved')

  // Over the relay, end to end with the real host.
  const link = new RelayLink({ cloudURL: CLOUD, computerId, hostSignKey: row.signKey, signer: desktop, dial: dialWebSocket, probe: probeUpgrade })
  const hello = (await link.call('hello', {}, 20_000)) as { computerId?: string; name?: string }
  check(hello.computerId === computerId, `hello over the relay names the real host (${hello.name})`)
  const people = (await link.call('people', {}, 20_000)) as { people?: unknown[] }
  check(Array.isArray(people.people), 'a second read method (people) answered')
  const lines: string[] = []
  link.subscribe('events', { since: 0, client: 'desktop-e2e' }, (l) => lines.push(l), () => {})
  await waitFor('the first event', () => lines.length >= 1)
  check(JSON.parse(lines[0]).type === 'hello', 'the event stream opened with the host hello')
  check(unb64url(row.signKey)?.length === 32, 'the key is the one the id was made from')
  link.close()
  await invitee(computerId, row.signKey)
  console.log('\ndesktop relay e2e (real host) passed')
}

/** A second account joins as a member of `house` with the desktop's own join code (shared/relay-access + invite-join). */
async function invitee(computerId: string, signKey: string) {
  console.log('\ninvitee:')
  const made = await loopback('createInvite', { name: 'Mia', agents: { house: 'member' } })
  // A dev host hands out the env scheme (kith-dev://), a main one kith://.
  const link = parseInviteLink(made.appLink, ['kith', 'kith-dev'])
  check(link && link.computerId === computerId, 'the owner made an invite for the real host')
  const first = [made.webLink, made.pageLink, made.appLink].find((l) => typeof l === 'string')
  check(made.link === first, 'link is the web link, else the page link, else the app link')
  check(made.kithLink === made.appLink, 'kithLink is the app link')
  check(typeof made.pageLink !== 'string' || /^https?:\/\/[^#]+\/i\/[^#]+#t=/.test(made.pageLink), 'pageLink keeps the secret in the fragment')
  check(typeof made.webLink !== 'string' || /^https?:\/\/[^#]+#/.test(made.webLink), 'webLink keeps the secret in the fragment')
  if (!link) return

  const token = await clerkToken('user_inviteee2e')
  const device = signerFromSeed(Buffer.alloc(32, 0x62)) // fake, low entropy
  const req = (method: string, path: string, body: unknown, signed: boolean) =>
    cloud(method, `/${path}`, { token, signer: signed ? device : undefined, body }).then((r) =>
      r.status >= 200 && r.status < 300 ? ({ ok: true, value: r.body } as const) : ({ ok: false, error: { status: r.status, code: r.body?.error?.code ?? `http${r.status}`, message: r.body?.error?.message ?? null } } as const),
    )
  check((await req('POST', 'v1/devices', { name: 'Invitee e2e', platform: 'macos' }, true)).ok, 'the invitee registered its device key')
  const links = new Map<string, RelayLink>()
  const linkOf = (id: string, key: string) => {
    if (!links.has(id)) links.set(id, new RelayLink({ cloudURL: CLOUD, computerId: id, hostSignKey: key, signer: device, dial: dialWebSocket, probe: probeUpgrade }))
    return links.get(id)!
  }
  const api: RelayApi = {
    computers: async () => {
      const r = await req('GET', 'v1/computers', undefined, true)
      return r.ok ? { ok: true, value: (r.value as any).computers } : r
    },
    requestAccess: (id, key) => requestAccess({ request: req, signer: device, computerId: id, signKey: key, cancelled: () => false, created: () => {}, pollMs: 300 }),
    accessStatus: async (id) => {
      const r = await req('GET', `v1/access-requests/${id}`, undefined, false)
      return r.ok ? { ok: true, value: (r.value as any).status } : r
    },
    cancelAccess: async () => {},
    state: async (id, key) => {
      const l = linkOf(id, key)
      l.start()
      return l.state
    },
    call: (id, key, method, body, ms) => linkOf(id, key).call(method, body, ms),
  }
  // The Mac's owner approves what the invitee asks once the Mac shows its digits (after the device revealed its nonce).
  let approving = true
  void (async () => {
    while (approving) {
      for (const r of (await loopback('accessRequests').catch(() => ({ requests: [] }))).requests.filter((x: any) => x.code)) await loopback('decideAccessRequest', { requestId: r.requestId, approve: true }).catch(() => {})
      await sleep(300)
    }
  })()
  const order: string[] = []
  const steps: JoinState[] = []
  const deps = relayJoinDeps(api, {
    signedIn: () => true,
    waitForSignIn: async () => {},
    accept: async (inviteId, h1) => {
      const r = await req('POST', `v1/invites/${inviteId}/accept`, { token: h1 }, false)
      if (!r.ok) throw r.error
      return r.value as { computerId: string }
    },
    keepToken: async () => void order.push('kept'),
  })
  const end = await runJoin(link, deps, (st) => steps.push(st))
  approving = false
  check(end.step === 'done', `the join finished (${steps.map((x) => x.step).join(' > ')}${end.message ? `: ${end.message}` : ''})`)
  check(order.length === 1, 'the invite token was kept on this device')

  const mac = linkOf(computerId, signKey)
  const hello = (await mac.call('hello', {}, 20_000)) as { grantedAgents?: { agent: string; role: string }[] }
  check(JSON.stringify(hello.grantedAgents?.map((g) => [g.agent, g.role])) === JSON.stringify([['house', 'member']]), 'hello.grantedAgents names house as member')
  const chat = await waitFor('startChat to work', async () => {
    try {
      return (await mac.call('startChat', { agent: 'house' }, 30_000)) as { bot?: { id: string; hqAgent?: string } }
    } catch {
      return undefined
    }
  }, 30_000)
  check(chat.bot?.hqAgent === 'house', 'startChat made the invitee their own house chat')
  let refused = ''
  try {
    await mac.call('devices', {}, 20_000)
  } catch (e) {
    refused = String((e as { message?: string }).message)
  }
  check(refused !== '', `an owner-only call is refused to the member (${refused})`)
  // The real kith-guest binary, when the job built it (the bot-guests e2e); the owner's and this member's bots.
  if (process.env.KITH_GUEST_BIN) {
    await botGuests({
      guestBin: process.env.KITH_GUEST_BIN,
      cloud: CLOUD,
      tmp,
      loopback,
      member: (method, body) => mac.call(method, body ?? {}, 30_000),
      stopHost: () => stop(hostChild!),
      check,
    })
  }
  for (const l of links.values()) l.close()
}

try {
  await main()
} catch (e) {
  console.error('\ndesktop relay e2e (real host) failed:', e)
  for (const [name, tail] of tails) console.error(`\n--- last ${tail.length} lines of ${name} ---\n${tail.join('\n')}`)
  process.exitCode = 1
} finally {
  await Promise.all(children.map(stop))
  rmSync(tmp, { recursive: true, force: true })
  // Relay links a failed check left open keep Node running until the job's timeout (shown as cancelled): end here.
  process.exit()
}
