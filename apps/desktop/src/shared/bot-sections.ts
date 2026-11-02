import type { KithInfo } from './kith-info.ts'

/**
 * Which of Kith's own cards an hq bot's page still shows. The Kith sections (serve-bot-details) carry
 * the same facts, read from the agent's package, so each fact shows once and Kith's copy wins.
 * A card goes only once Kith has the fact on screen: while the details load, fail or are missing,
 * every card stays, and a plain bot (no `hqAgent`) always shows all of them.
 */
export interface BotSections {
  /** "Standing instructions": Kith's About shows the agent's role. */
  instructions: boolean
  /** The agent (backend) picker: Kith's Model & permissions shows the harness. */
  backendPicker: boolean
  /**
   * The harness's own notes tab in Memory. For an hq bot it is Claude Code's MEMORY.md of the bot's working
   * folder (the host's `projectMemory`), not the agent's memory: Kith's "Agent memory" lists the real files.
   * Gone for every hq bot, loaded or not.
   */
  harnessMemoryTab: boolean
  /**
   * Kith's own permission, model, effort and Fast mode controls. hq is the one source for an hq bot's settings (the
   * host ignores its stored copy), so they go and the `hqSettings` rows take their place. A host that predates the
   * `kithInfo` call can't serve those, so the controls stay there.
   */
  kithSettings: boolean
}

export const ALL_SECTIONS: BotSections = { instructions: true, backendPicker: true, harnessMemoryTab: true, kithSettings: true }

export function botSections(
  hqAgent: string | null | undefined,
  info: Pick<KithInfo, 'description' | 'role' | 'harness' | 'errors'> | null,
  unsupportedHost = false,
): BotSections {
  if (!hqAgent) return ALL_SECTIONS
  const kithSettings = unsupportedHost
  if (!info) return { ...ALL_SECTIONS, harnessMemoryTab: false, kithSettings }
  return {
    instructions: !(info.role || info.description) || info.errors.role !== undefined,
    backendPicker: !info.harness,
    harnessMemoryTab: false,
    kithSettings,
  }
}

/** Claude Code's own permission modes, in the order the picker lists them. Kith passes the pick to the agent unchanged. */
export const PERMISSION_MODES: { id: string; label: string; detail: string }[] = [
  { id: 'default', label: 'Default', detail: 'Asks the first time each tool is used' },
  { id: 'acceptEdits', label: 'Accept edits', detail: 'File edits are accepted without asking; other actions ask' },
  { id: 'plan', label: 'Plan only', detail: 'Read-only planning; changes nothing' },
  { id: 'auto', label: 'Auto', detail: 'A classifier approves safe actions; risky ones ask or are blocked' },
  { id: 'bypassPermissions', label: 'Bypass permissions', detail: 'No permission checks' },
]

/** The Mode menu's wording (the same as the iPhone's), by mode id. A mode with no entry shows its PERMISSION_MODES text. */
const PERMISSION_MENU_TEXT: Record<string, { label: string; detail: string }> = {
  default: { label: 'Default', detail: 'Asks before edits and commands' },
  acceptEdits: { label: 'Accept edits', detail: 'Edits files, asks before commands' },
  plan: { label: 'Plan', detail: 'Reads and plans, changes nothing' },
  auto: { label: 'Auto', detail: 'Decides itself, risky steps are checked' },
  bypassPermissions: { label: 'Bypass permissions', detail: 'Never asks. Only in a sandbox' },
}

/** The Mode menu's items: PERMISSION_MODES, so a mode that leaves it leaves the menu. */
export function permissionMenu(): { id: string; label: string; detail: string; risky: boolean }[] {
  return PERMISSION_MODES.map((m) => ({ ...m, ...PERMISSION_MENU_TEXT[m.id], risky: m.id === 'bypassPermissions' }))
}

/** A stored mode as the picker knows it: the old `ask` and `manual` are `default`, a stored `dontAsk` is `auto`. */
export function permissionMode(mode: string | null | undefined): string {
  if (mode === 'ask' || mode === 'manual') return 'default'
  return mode === 'dontAsk' ? 'auto' : (mode ?? 'auto')
}

/** What a voice call can run on (`kith <agent> set call-model`); `default` follows this Mac's global call model (sonnet unless changed), `own` is the agent's own model. */
const CALL_MODEL_NAMES: { id: string; label: string }[] = [
  { id: 'sonnet', label: 'Sonnet' },
  { id: 'haiku', label: 'Haiku' },
  { id: 'opus', label: 'Opus' },
  { id: 'own', label: "Agent's own model" },
]

/** The picker's choices; `defaultIs` is the global call model `default` stands for. */
export function callModels(defaultIs: string | null): { id: string; label: string }[] {
  return [{ id: 'default', label: `Default (${callModelName(defaultIs ?? 'sonnet')})` }, ...CALL_MODEL_NAMES]
}

function callModelName(id: string): string {
  return CALL_MODEL_NAMES.find((m) => m.id === id)?.label.toLowerCase() ?? id
}

export interface HqSetting {
  key: 'permission' | 'callModel' | 'harness' | 'model' | 'effort' | 'fastMode'
  label: string
  /** What hq reports, or plain words when it has nothing. */
  value: string
  /** Written through to hq. The rest hq has no setting for: read-only, "Set in Kith". */
  editable: boolean
  hint: string
}

/** Effort levels `kith <agent> set effort` takes, plus `default` (forget the choice). */
export const EFFORT_LEVELS: { id: string; label: string }[] = [
  { id: 'default', label: "Agent's default" },
  { id: 'low', label: 'Low' },
  { id: 'medium', label: 'Medium' },
  { id: 'high', label: 'High' },
  { id: 'xhigh', label: 'Extra high' },
  { id: 'max', label: 'Max' },
]

/** The harnesses the picker offers for a kith agent; the agent's current one is added when it is another. */
export const HARNESSES: { id: string; label: string }[] = [
  { id: 'claude', label: 'Claude' },
  { id: 'codex', label: 'Codex' },
]

export const SET_IN_KITH = 'Set in Kith'

/**
 * The settings of an hq bot, once, read from hq (`info` is `kith <agent> --info-json`; null while it loads or failed).
 * The permission mode and the call model have hq settings to write to (`kith <agent> set permission|call-model`); the
 * model and effort are the agent package's own, and hq has no Fast mode.
 */
export function hqSettings(info: Pick<KithInfo, 'permissionMode' | 'callModel' | 'model' | 'effort'> & { harness?: string | null; callModelDefault?: string | null } | null): HqSetting[] {
  return [
    {
      key: 'permission', label: 'Permissions', editable: info !== null,
      value: info ? permissionMode(info.permissionMode) : '…',
      hint: 'Kept by Kith on this computer. Every chat of this agent uses it from its next turn.',
    },
    {
      key: 'callModel', label: 'Call model', editable: info !== null,
      value: info ? (info.callModel ?? 'default') : '…',
      hint: 'The model that answers when you talk to this agent in a voice call. Typed messages use the agent\'s own model.',
    },
    {
      key: 'harness', label: 'Harness', editable: info !== null,
      value: info ? (info.harness ?? 'claude') : '…',
      hint: 'The program that runs this agent on this computer. Switching drops a model the new harness can\'t run.',
    },
    {
      key: 'model', label: 'Model', editable: info !== null,
      value: info ? (info.model ?? 'default') : '…',
      hint: 'Kept by Kith for this agent. Every chat of this agent uses it from its next turn.',
    },
    {
      key: 'effort', label: 'Effort', editable: info !== null,
      value: info ? (info.effort ?? 'default') : '…',
      hint: 'How hard the agent thinks. Kept by Kith for this agent.',
    },
    { key: 'fastMode', label: 'Fast mode', editable: false, value: 'Off', hint: `${SET_IN_KITH}: hq has no Fast mode.` },
  ]
}

/** The picker's label for a call model id; one this app doesn't know shows as it is. */
export function callModelLabel(id: string, defaultIs: string | null = null): string {
  return callModels(defaultIs).find((m) => m.id === id)?.label ?? id
}

/** The picker's label for a mode id; an id this app doesn't know shows as it is. */
export function permissionLabel(mode: string): string {
  return PERMISSION_MODES.find((m) => m.id === permissionMode(mode))?.label ?? mode
}
