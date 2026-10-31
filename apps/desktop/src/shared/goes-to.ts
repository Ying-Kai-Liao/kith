/** Where the home box's message goes, before it is sent. The app picks; there is no router on the host. */

export interface GoesToAgent {
  key: string
  name: string
  /** When the agent's chat last moved; the recency order. */
  lastAt: number
  /** The agent of the chat open now. */
  selected?: boolean
}

export interface GoesTo {
  /** The agent the message goes to; null when there is none, or an @name matched no agent. */
  target: GoesToAgent | null
  /** `mention`: the user typed @name; `auto`: the app picked. */
  how: 'mention' | 'auto' | 'unknown' | 'none'
  /** The next two most recent other agents, to tap instead. Empty for a mention. */
  alternatives: GoesToAgent[]
  /** Every agent, most recent first, for the full list. */
  all: GoesToAgent[]
  /** The message without the leading @name. */
  text: string
}

const fold = (s: string) => s.trim().toLowerCase()

/**
 * Pick order: @mention > the agent of the selected chat > the most recent chat's agent > the first agent.
 * `agents` must be the owner's own agents only (no groups, no outside bots). Alternatives are the next two most
 * recent distinct agents after the pick. `override` is an agent the user tapped in the pill.
 */
export function goesTo(agents: GoesToAgent[], query: string, override: string | null = null): GoesTo {
  // Stable: ties keep the roster's order, so "first agent" holds when nothing has been said yet.
  const all = agents.map((a, i) => ({ a, i })).sort((x, y) => y.a.lastAt - x.a.lastAt || x.i - y.i).map((x) => x.a)
  const trimmed = query.trimStart()
  if (trimmed.startsWith('@')) {
    const rest = trimmed.slice(1)
    // Longest name first, so "@Ada Lovelace hi" beats an agent named "Ada".
    const hit = [...all].sort((x, y) => y.name.length - x.name.length).find((a) => {
      const n = a.name.trim().length
      return n > 0 && fold(rest.slice(0, n)) === fold(a.name) && (rest.length === n || /\s/.test(rest[n]))
    })
    if (!hit) return { target: null, how: 'unknown', alternatives: [], all, text: query }
    return { target: hit, how: 'mention', alternatives: [], all, text: rest.slice(hit.name.trim().length).trim() }
  }
  const text = query.trim()
  const chosen = (override && all.find((a) => a.key === override)) || all.find((a) => a.selected) || all[0] || null
  if (!chosen) return { target: null, how: 'none', alternatives: [], all, text }
  return { target: chosen, how: 'auto', alternatives: all.filter((a) => a !== chosen).slice(0, 2), all, text }
}
