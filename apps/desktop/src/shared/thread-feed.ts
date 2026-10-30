import type { Entry } from './models.ts'

/** An expanded run of steps shows its newest this many and folds the rest into "N earlier steps" (a long task can run hundreds). Same as iOS. */
export const VISIBLE_STEPS = 20

/** `mcp__kith-files__send_file` -> the tool part, "send_file"; null when the text is not an MCP name. */
function mcpTool(text: string): { tool: string; rest: string } | null {
  const m = /^mcp__.+?__(\S+)\s*(.*)$/.exec(text.trim())
  return m ? { tool: m[1]!, rest: m[2]! } : null
}

/** "send_file" -> "Send file". */
const humanize = (tool: string): string => {
  const words = tool.replace(/[_-]+/g, ' ').trim()
  return words.charAt(0).toUpperCase() + words.slice(1)
}

/**
 * A tool entry for a kith-files tool that delivers a file or image to the chat (`send_file`): the delivered item says
 * it, so the step isn't listed. Older entries have no tool-name field, so the title (or command) is what's matched.
 */
export function isDeliveryTool(entry: Entry): boolean {
  if (entry.kind !== 'tool') return false
  const d = entry.data
  return [d.title, d.command].some((t) => {
    const first = (t ?? '').split('\n')[0] ?? ''
    return /^mcp__kith-files__send_file(\s|$)/.test(first.trim()) || /^send_file(\s|$)/.test(first.trim())
  })
}

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
  const title = (d.title ?? '').split('\n')[0] ?? ''
  const mcp = mcpTool(title)
  const object = first
    ? `${first.path.split('/').pop()}${diffs.length > 1 ? ` +${diffs.length - 1}` : ''}`
    : mcp ? mcp.rest : withoutVerb(title, d.toolKind)
  const added = diffs.reduce((n, x) => n + x.added, 0)
  const removed = diffs.reduce((n, x) => n + x.removed, 0)
  const state = d.status === 'completed' ? 'done' : d.status === 'failed' ? 'error' : 'running'
  const result = state === 'done' ? (diffs.length ? `+${added} -${removed}` : 'done') : state === 'error' ? 'failed' : undefined
  const command = d.command || (d.toolKind === 'execute' ? d.title : undefined) || undefined
  const patches = diffs.map((x) => x.patch).join('\n')
  const output = [d.output, patches].filter(Boolean).join('\n\n') || undefined
  // A raw `mcp__server__tool` name never shows: the tool part is the verb.
  const verb = mcp ? humanize(mcp.tool) : VERBS[d.toolKind ?? ''] ?? 'Used'
  return { verb, object, result, state, command: command && mcpTool(command) ? undefined : command, output }
}
