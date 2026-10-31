import type { Bot } from './models.ts'

/** The host's limits (store/model.rs): a room has at most this many labels, each at most this long. */
export const MAX_LABELS = 5
export const MAX_LABEL_CHARS = 24

/** Labels typed as "launch, q4 plan": lowercase, trimmed, no blanks or repeats, cut to what the host takes. */
export function parseLabels(text: string): string[] {
  const seen: string[] = []
  for (const part of text.split(/[,\n]/)) {
    const label = part.trim().replace(/\s+/g, ' ').toLowerCase().slice(0, MAX_LABEL_CHARS)
    if (label && !seen.includes(label)) seen.push(label)
  }
  return seen.slice(0, MAX_LABELS)
}

/** The labels rooms use, most used first (then alphabetical), for the filter row. */
export function labelsInUse(bots: readonly Bot[]): string[] {
  const counts = new Map<string, number>()
  for (const bot of bots) {
    if (bot.kind !== 'group') continue
    for (const label of bot.labels ?? []) counts.set(label, (counts.get(label) ?? 0) + 1)
  }
  return [...counts.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([label]) => label)
}

/** Whether a row shows under the chosen label; no label chosen shows everything. Only rooms carry labels. */
export function hasLabel(bot: Bot, label: string | null): boolean {
  return label === null || (bot.kind === 'group' && (bot.labels ?? []).includes(label))
}
