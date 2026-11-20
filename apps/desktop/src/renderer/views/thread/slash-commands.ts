// The composer's `/` commands, listed while the draft is just `/partial`: a skill fills in `/<id> `, and the host turns that message into "read this skill first"
// (host/src/market/skills.rs `expand_command`). docs/features/marketplace.md, "Commands".
import type { Bot } from '../../../shared/models.ts'

// Fast mode was the one built-in: a kith agent has none (the CLI has no such setting), so only skills remain.
export type SlashAction = { kind: 'skill'; id: string }

export interface SlashCommand {
  /** What follows the `/`. */
  name: string
  icon: string
  title: string
  detail: string
  warning: boolean
  action: SlashAction
}

/** The command name being typed after a leading `/`, before any space; otherwise null. */
export function slashQuery(draft: string): string | null {
  return /^\/\S*$/.test(draft) ? draft.slice(1) : null
}

/** The bot's skills (ids in `bot.skills`), whose name or title holds `query`. */
export function slashCommands(bot: Bot | null, query: string, installedSkills: { id: string; name: string; description: string }[]): SlashCommand[] {
  if (!bot || bot.kind === 'group') return []
  const skills = installedSkills
    .filter((s) => bot.skills.includes(s.id))
    .map((s): SlashCommand => ({ name: s.id, icon: 'book.pages', title: s.name, detail: s.description, warning: false, action: { kind: 'skill', id: s.id } }))
  const q = query.toLowerCase()
  return skills.filter((c) => c.name.includes(q) || c.title.toLowerCase().includes(q))
}

/**
 * What Return does with `picked` highlighted: run it, except a skill whose name is already
 * typed out, which sends.
 */
export function returnRuns(picked: SlashCommand, draft: string): boolean {
  return picked.action.kind !== 'skill' || draft !== `/${picked.name}`
}
