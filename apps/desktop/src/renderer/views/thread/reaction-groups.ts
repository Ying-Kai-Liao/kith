export interface ReactionGroup {
  emoji: string
  count: number
  /** entry.data.reactions is the user's own list, so every chip is theirs. */
  mine: boolean
}

/** The user's reactions as chips: identical emoji grouped with a count, in first-seen order (old data may repeat an emoji). */
export function groupReactions(reactions: readonly string[] | undefined): ReactionGroup[] {
  const counts = new Map<string, number>()
  for (const e of reactions ?? []) counts.set(e, (counts.get(e) ?? 0) + 1)
  return [...counts].map(([emoji, count]) => ({ emoji, count, mine: true }))
}
