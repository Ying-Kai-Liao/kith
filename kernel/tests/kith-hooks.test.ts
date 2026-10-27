// kith-hooks.test — the Claude Code plugin (adapters/claude-code/hooks/register.tsx), driven with a fake engine:
//   a helper's report is never replaced (SubagentStop never blocks; a stale session file gets a stub written from the
//   helper's last message), mcp__kith__invoke returns the report, a helper in the owner's session registers with its own
//   permissions and one in a guest's does not, every helper checks MCP tools against its own grants.
//
//   bun test kernel/tests/kith-hooks.test.ts
import { expect, test } from 'bun:test'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { register } from '../adapters/claude-code/hooks/register.tsx'

type Handler = (...args: any[]) => any

import { statSync } from 'node:fs'
const await_stat = (p: string) => statSync(p).mtimeMs

function setup(env: Record<string, string> = {}, extra?: (argv: string[]) => { exitCode: number; stdout: string; stderr: string } | undefined) {
  const root = mkdtempSync(join(tmpdir(), 'kith-hooks-'))
  const kernel = join(root, 'kernel')
  const agents = join(root, 'agents')
  const state = join(root, 'state')
  mkdirSync(join(kernel, 'bin'), { recursive: true })
  mkdirSync(join(agents, 'builder/role'), { recursive: true })
  mkdirSync(state, { recursive: true })
  writeFileSync(join(kernel, 'bin/kith-pkg'), '')
  writeFileSync(join(kernel, 'PROTOCOL.md'), 'protocol')
  writeFileSync(join(agents, 'builder/role/agent.md'), '---\nname: builder\ndescription: builds\n---\nYou build.\n')
  const handlers: Record<string, Handler[]> = {}
  const on = (name: string, ...rest: any[]) => {
    const fn = rest[rest.length - 1] as Handler
    ;(handlers[name] ??= []).push(fn)
    return { catch: (c: Handler) => { (fn as any).onError = c; return undefined } }
  }
  const registered: any[] = []
  const calls: string[][] = []
  let nextId = 0
  const $ = {
    env: { get: async (k: string) => (k === 'HOME' ? root : env[k]) },
    fs: {
      exists: async (p: string) => existsSync(p),
      read: async (p: string) => readFileSync(p, 'utf8'),
      write: async (p: string, t: string) => { mkdirSync(join(p, '..'), { recursive: true }); writeFileSync(p, t) },
      list: async (p: string) =>
        (await import('node:fs')).readdirSync(p).map((name) => ({ name, mtimeMs: (await_stat(join(p, name))) })),
      stat: async (p: string) => ({ mtimeMs: (await import('node:fs')).statSync(p).mtimeMs }),
    },
    process: {
      run: async (argv: string[]) => {
        const j = argv.join(' ')
        calls.push(argv)
        const x = extra?.(argv)
        if (x) return x
        const ok = (stdout: string) => ({ exitCode: 0, stdout, stderr: '' })
        if (j.includes('kith_paths.py'))
          return ok(JSON.stringify({ KITH_STATE: state, KITH_AGENTS: agents, KITH_STATE_REAL: state, KITH_AGENTS_REAL: agents }))
        if (j.includes('kith-pkg list')) return ok(JSON.stringify({ builder: { dir: join(agents, 'builder/role') } }))
        if (j.includes('kith-pkg resolve')) return ok('{}')
        if (j.includes('permission_mode')) return ok('manual\n')
        if (j.includes('project-id')) return ok('kith\n')
        return { exitCode: 1, stdout: '', stderr: '' }
      },
    },
    agent: {
      register: async (spec: any) => { registered.push(spec) },
      spawn: async () => ({ agentId: `agent-${++nextId}`, model: 'x' }),
    },
    command: { register: async () => {} },
    tool: { register: async () => {} },
    ui: { toast: () => {} },
    session: { cwd: async () => root },
  }
  register(on as any, { kith_path: kernel } as any)
  const fire = async (name: string, e: any) => {
    const list = handlers[name] ?? []
    const next = async (x: any) => x
    return list[0]($, e, next)
  }
  return { root, agents, state, $, registered, handlers, fire, calls }
}

test('SubagentStop never blocks and writes a stub when the session file is stale', async () => {
  const t = setup()
  await t.fire('session.start', { cwd: t.root })
  await t.fire('agent.spawn', { subagentType: 'kith:builder', prompt: 'do the thing', cwd: t.root })
  const spawn = t.handlers['agent.spawn'][0]
  const started = await spawn(t.$, { subagentType: 'kith:builder', prompt: 'do the thing', cwd: t.root }, async (x: any) => ({ agentId: 'a1', prompt: x.prompt }))
  expect(started.prompt).toContain('Before your final report, write')
  const out = await t.fire('classic.SubagentStop', { agent_id: 'a1', last_assistant_message: 'Report: all done', stop_hook_active: false })
  expect(out.block).toBeUndefined()
  const file = join(t.agents, 'builder/memory/sessions/kith.md')
  expect(readFileSync(file, 'utf8')).toContain('Report: all done')
})

test('SubagentStop leaves a session file the helper wrote', async () => {
  const t = setup()
  await t.fire('session.start', { cwd: t.root })
  const spawn = t.handlers['agent.spawn'][0]
  await spawn(t.$, { subagentType: 'kith:builder', prompt: 'x', cwd: t.root }, async () => ({ agentId: 'a2' }))
  const file = join(t.agents, 'builder/memory/sessions/kith.md')
  mkdirSync(join(file, '..'), { recursive: true })
  writeFileSync(file, 'mine')
  utimesSync(file, new Date(Date.now() + 5000), new Date(Date.now() + 5000))
  const out = await t.fire('classic.SubagentStop', { agent_id: 'a2', last_assistant_message: 'done' })
  expect(out.block).toBeUndefined()
  expect(readFileSync(file, 'utf8')).toBe('mine')
})

test('a session file of another project written during the run counts: no stub', async () => {
  const t = setup()
  await t.fire('session.start', { cwd: t.root })
  const spawn = t.handlers['agent.spawn'][0]
  await spawn(t.$, { subagentType: 'kith:builder', prompt: 'x', cwd: t.root }, async () => ({ agentId: 'a3' }))
  const other = join(t.agents, 'builder/memory/sessions/renamed.md')
  mkdirSync(join(other, '..'), { recursive: true })
  writeFileSync(other, 'mine')
  utimesSync(other, new Date(Date.now() + 5000), new Date(Date.now() + 5000))   // coarse mtimes on CI
  await t.fire('classic.SubagentStop', { agent_id: 'a3', last_assistant_message: 'done' })
  expect(existsSync(join(t.agents, 'builder/memory/sessions/kith.md'))).toBe(false)
})

test('mcp__kith__invoke waits for the helper and returns its report', async () => {
  const t = setup()
  await t.fire('session.start', { cwd: t.root })
  const invoke = t.handlers['tool.call'][0]
  const pending = invoke(t.$, { agent: 'builder', task: 'go' })
  await new Promise((r) => setTimeout(r, 20))
  await t.fire('turn.complete', { agentId: 'agent-1', answer: 'the report' })
  expect((await pending).result).toBe('the report')
  // a report that came before the call looked for it
  t.$.agent.spawn = async () => ({ agentId: 'early' }) as any
  await t.fire('turn.complete', { agentId: 'early', answer: 'first' })
  expect((await invoke(t.$, { agent: 'builder', task: 'again' })).result).toBe('first')
})

test('a helper in the owner session has its own permissions; in a guest session it does not', async () => {
  const owner = setup()
  await owner.fire('session.start', { cwd: owner.root })
  const spec = owner.registered[0]
  expect(spec.permissionMode).toBe('default')   // the agent's `manual`
  const cmd: string = spec.hooks.PreToolUse[0].hooks[0].command
  expect(cmd).toContain('kith-helper-allow')
  expect(cmd).toContain(join(owner.agents, 'builder'))
  expect(cmd).toContain(join(owner.agents, 'builder/role/skills/learned'))

  const guest = setup({ KITH_PERSON: 'guest' })
  await guest.fire('session.start', { cwd: guest.root })
  expect(guest.registered[0].permissionMode).toBeUndefined()
  expect(guest.registered[0].hooks.PreToolUse.map((h: any) => h.matcher)).toEqual(['mcp__.*'])
})

// The caller's MCP servers are the helper's too: its own grants decide (kith-auth helper-mcp), asked in the owner's session.
test('every helper checks MCP tools against its own grants: ask for the owner, deny for anyone else', async () => {
  const gate = (t: ReturnType<typeof setup>) => {
    const h = t.registered[0].hooks.PreToolUse.find((x: any) => x.matcher === 'mcp__.*')
    return h.hooks[0].command as string
  }
  const owner = setup()
  await owner.fire('session.start', { cwd: owner.root })
  expect(gate(owner)).toBe(`'${join(owner.root, 'kernel')}/bin/kith-auth' helper-mcp 'builder' ask`)
  const guest = setup({ KITH_PERSON: 'mia', KITH_OWNER: '0' })
  await guest.fire('session.start', { cwd: guest.root })
  expect(gate(guest)).toEndWith(` helper-mcp 'builder' deny`)
  const bad = setup({ KITH_PERSON: '../x', KITH_OWNER: '0' })
  await bad.fire('session.start', { cwd: bad.root })
  expect(gate(bad)).toEndWith(' deny')
})

test('KITH_OWNER decides: an owner run whose KITH_PERSON names the owner still gets the helper permissions', async () => {
  const owner = setup({ KITH_PERSON: 'kai', KITH_OWNER: '1' })
  await owner.fire('session.start', { cwd: owner.root })
  expect(owner.registered[0].permissionMode).toBe('default')
  const guest = setup({ KITH_PERSON: 'guest', KITH_OWNER: '0' })
  await guest.fire('session.start', { cwd: guest.root })
  expect(guest.registered[0].permissionMode).toBeUndefined()
  expect(guest.registered[0].hooks.PreToolUse).toHaveLength(1)
})

test('mcp__kith__invoke returns at SubagentStop when no turn.complete came', async () => {
  const t = setup()
  await t.fire('session.start', { cwd: t.root })
  const pending = t.handlers['tool.call'][0](t.$, { agent: 'builder', task: 'go' })
  await new Promise((r) => setTimeout(r, 20))
  await t.fire('classic.SubagentStop', { agent_id: 'agent-1', last_assistant_message: 'stopped report' })
  expect((await pending).result).toBe('stopped report')
})

test('kith-helper-allow allows writes under its folders only, symlinks and .. resolved', () => {
  const root = mkdtempSync(join(tmpdir(), 'kith-allow-'))
  mkdirSync(join(root, 'life/memory'), { recursive: true })
  mkdirSync(join(root, 'other'), { recursive: true })
  symlinkSync(join(root, 'life'), join(root, 'link'))
  const run = (file: string) =>
    Bun.spawnSync(['python3', join(import.meta.dir, '../bin/kith-helper-allow'), join(root, 'life')], {
      stdin: new TextEncoder().encode(JSON.stringify({ tool_name: 'Write', tool_input: { file_path: file } })),
    }).stdout.toString()
  expect(run(join(root, 'life/memory/sessions/x.md'))).toContain('"allow"')
  expect(run(join(root, 'link/memory/x.md'))).toContain('"allow"')
  expect(run(join(root, 'life/../other/x.md'))).toBe('')
  expect(run(join(root, 'other/x.md'))).toBe('')
  expect(run('relative.md')).toBe('')
})

// Helper memory follows the run's person (R6): a helper spawned in a guest's run never gets the owner's memory.
const withFacets = (resolve: object | null, facets: string | null) => (argv: string[]) => {
  const j = argv.join(' ')
  if (j.includes('kith-auth') && j.includes(' may ')) return { exitCode: 0, stdout: '', stderr: '' }
  if (j.includes('kith-auth') && j.includes('resolve'))
    return resolve ? { exitCode: 0, stdout: JSON.stringify(resolve), stderr: '' } : { exitCode: 1, stdout: '', stderr: 'no' }
  if (j.includes('kith_facets.py'))
    return facets !== null ? { exitCode: 0, stdout: JSON.stringify({ block: facets, attrs: '' }), stderr: '' } : { exitCode: 1, stdout: '', stderr: 'bad' }
  return undefined
}

async function spawnHelper(t: ReturnType<typeof setup>) {
  writeFileSync(join(t.agents, 'builder/role/agent.md'),
    '---\nname: builder\ndescription: builds\n---\nmem={{MEMORY_DIR}} shared={{SHARED_MEMORY}} files={{FILES_DIR}} who={{PERSON_FILE}}\n')
  mkdirSync(join(t.agents, 'builder/memory/sessions'), { recursive: true })
  writeFileSync(join(t.agents, 'builder/memory/MEMORY.md'), 'OWNER-SECRET')
  writeFileSync(join(t.agents, 'builder/memory/sessions/kith.md'), 'OWNER-SESSION')
  await t.fire('session.start', { cwd: t.root })
  const spawn = t.handlers['agent.spawn'][0]
  return spawn(t.$, { subagentType: 'kith:builder', prompt: 'do the thing', cwd: t.root }, async (x: any) => ({ agentId: 'a1', prompt: x.prompt }))
}

test('owner helper gets the owner memory', async () => {
  const t = setup()
  const started = await spawnHelper(t)
  expect(started.prompt).toContain('OWNER-SECRET')
  expect(started.prompt).toContain(join(t.agents, 'builder/memory/sessions/kith.md'))
  expect(t.calls.some((c) => c.join(' ').includes('kith_facets'))).toBe(false)
})

test('a person helper gets the person memory and none of the owner\'s', async () => {
  const t = setup({ KITH_PERSON: 'mia', KITH_OWNER: '0' },
    withFacets({ allowed: 1, owner: 0, person: 'mia', rooms: ['r1'] }, '<memory>MIA-NOTES</memory>'))
  const started = await spawnHelper(t)
  expect(started.prompt).toContain('MIA-NOTES')
  expect(started.prompt).not.toContain('OWNER-SECRET')
  expect(started.prompt).not.toContain(join(t.agents, 'builder/memory/sessions'))
  expect(started.prompt).toContain(join(t.agents, 'builder/memory/people/mia/sessions/kith.md'))
  const f = t.calls.find((c) => c.join(' ').includes('kith_facets'))!
  expect(f[f.indexOf('--kind') + 1]).toBe('person')
  expect(f[f.indexOf('--person') + 1]).toBe('mia')
  expect(f).toContain('--json')
  expect(f[f.indexOf('--person-file') + 1]).toBe(join(t.state, 'people/mia.md'))
  // the stub lands in the person's sessions dir, never the owner's
  await t.fire('classic.SubagentStop', { agent_id: 'a1', last_assistant_message: 'Report: mia done' })
  expect(readFileSync(join(t.agents, 'builder/memory/people/mia/sessions/kith.md'), 'utf8')).toContain('mia done')
  expect(readFileSync(join(t.agents, 'builder/memory/sessions/kith.md'), 'utf8')).toBe('OWNER-SESSION')
})

test('a bot guest helper is loaded as a guest', async () => {
  const t = setup({ KITH_PERSON: 'bot-ab12', KITH_OWNER: '0' },
    withFacets({ allowed: 1, owner: 0, person: 'bot-ab12', rooms: [] }, 'BOT-NOTES'))
  const started = await spawnHelper(t)
  expect(started.prompt).toContain('BOT-NOTES')
  expect(started.prompt).not.toContain('OWNER-SECRET')
  const f = t.calls.find((c) => c.join(' ').includes('kith_facets'))!
  expect(f[f.indexOf('--kind') + 1]).toBe('guest')
})

test('kith-auth or facets failing gives no memory, never the owner\'s', async () => {
  for (const extra of [withFacets(null, 'X'), withFacets({ allowed: 1, owner: 0, person: 'mia', rooms: [] }, null),
                       withFacets({ allowed: 0, owner: 0, person: 'mia', rooms: [] }, 'X')]) {
    const t = setup({ KITH_PERSON: 'mia', KITH_OWNER: '0' }, extra)
    const started = await spawnHelper(t)
    expect(started.prompt).not.toContain('OWNER-SECRET')
    expect(started.prompt).not.toContain('long-term-memory')
    expect(started.prompt).toContain('do the thing')
  }
  const bad = setup({ KITH_PERSON: '../x', KITH_OWNER: '0' }, withFacets(null, 'X'))
  const started = await spawnHelper(bad)
  expect(started.prompt).not.toContain('OWNER-SECRET')
  expect(started.prompt).not.toContain('Before your final report')
})

test('the memory placeholders are filled for the owner and for a person', async () => {
  const o = setup()
  await spawnHelper(o)
  const life = join(o.agents, 'builder')
  expect(o.registered[0].prompt).toContain(`mem=${life}/memory shared=${life}/memory/shared files=${life}/files who=${o.state}/user.md`)
  const p = setup({ KITH_PERSON: 'mia', KITH_OWNER: '0' })
  await spawnHelper(p)
  const pl = join(p.agents, 'builder')
  const prompt = p.registered[p.registered.length - 1].prompt as string
  expect(prompt).toContain(`mem=${pl}/memory/people/mia shared=${pl}/memory/shared files=${pl}/files/people/mia who=${p.state}/people/mia.md`)
  expect(prompt).not.toContain('{{')
})
