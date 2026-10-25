import type { Entry } from '../../../shared/models.ts'

/** A notice that roots a thread (a bot guest's conversation) shows its chip; every other notice stays a plain line. */
export const noticeThreadChip = (e: Pick<Entry, 'kind' | 'data'>): boolean => e.kind === 'notice' && !!e.data.thread && e.data.thread.count > 0
