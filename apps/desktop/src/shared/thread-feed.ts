import type { Entry } from './models.ts'

/** Tool calls in a row beyond this many fold into "N earlier steps" (a long task can run hundreds). Same as iOS. */
export const VISIBLE_STEPS = 4

const VERBS: Record<string, string> = {
  read: 'Read', edit: 'Edited', delete: 'Deleted', move: 'Moved', search: 'Searched', execute: 'Ran', think: 'Thought', fetch: 'Fetched',
}

/** Words a host title may open with per kind ("Read src/pace.js"); the row already has the verb, so they are dropped from the object. */
const TITLE_VERBS: Record<string, string[]> = {
  read: ['read', 'reading'],
  edit: ['edit', 'edited', 'editing', 'write', 'wrote', 'writing', 'written'],
  delete: ['delete', 'deleted', 'deleting', 'remove', 'removed', 'removing'],
  move: ['move', 'moved', 'moving', 'rename', 'renamed', 'renaming'],
  search: ['search', 'searched', 'searching'],
  think: ['think', 'thought', 'thinking'],
  fetch: ['fetch', 'fetched', 'fetching'],
}

/** "Read src/pace.js" -> "src/pace.js" for a read row; a title that is only the verb stays as it is. */
function withoutVerb(title: string, kind: string | undefined): string {
  const m = /^(\S+)\s+(\S.*)$/.exec(title)
  return m && TITLE_VERBS[kind ?? '']?.includes(m[1]!.toLowerCase()) ? m[2]! : title
}

export interface ToolRowModel {
  verb: string
  object: string
  result?: string
  state: 'running' | 'done' | 'error'
  command?: string
  output?: string
}

/** One tool entry as one row: "Edited kith-sessions.py  +6 -2". */
export function toolRowOf(entry: Entry): ToolRowModel {
  const d = entry.data
  const diffs = d.diffs ?? []
  const first = diffs[0]
  const object = first
    ? `${first.path.split('/').pop()}${diffs.length > 1 ? ` +${diffs.length - 1}` : ''}`
    : withoutVerb((d.title ?? '').split('\n')[0] ?? '', d.toolKind)
  const added = diffs.reduce((n, x) => n + x.added, 0)
  const removed = diffs.reduce((n, x) => n + x.removed, 0)
  const state = d.status === 'completed' ? 'done' : d.status === 'failed' ? 'error' : 'running'
  const result = state === 'done' ? (diffs.length ? `+${added} -${removed}` : 'done') : state === 'error' ? 'failed' : undefined
  const command = d.command || (d.toolKind === 'execute' ? d.title : undefined) || undefined
  const patches = diffs.map((x) => x.patch).join('\n')
  const output = [d.output, patches].filter(Boolean).join('\n\n') || undefined
  return { verb: VERBS[d.toolKind ?? ''] ?? 'Used', object, result, state, command, output }
}
