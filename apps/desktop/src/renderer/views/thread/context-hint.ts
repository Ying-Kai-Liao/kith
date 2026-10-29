/** "31% context" for the line under a tapped reply; the host sets contextPercent only above its threshold. Nothing when absent or not a number. */
export function contextLabel(contextPercent: unknown): string | null {
  return typeof contextPercent === 'number' && Number.isFinite(contextPercent) ? `${Math.round(contextPercent)}% context` : null
}

/** The ring's fill 0..1 for a percent; past 100 it stays full. */
export function contextFill(contextPercent: unknown): number | null {
  return typeof contextPercent === 'number' && Number.isFinite(contextPercent) ? Math.min(1, Math.max(0, contextPercent / 100)) : null
}

/** The line under a tapped reply: its time, then the ring and label when the host sent a percent. */
export function contextLine(time: string, contextPercent: unknown): { time: string; fill: number | null; label: string | null } {
  return { time, fill: contextFill(contextPercent), label: contextLabel(contextPercent) }
}
