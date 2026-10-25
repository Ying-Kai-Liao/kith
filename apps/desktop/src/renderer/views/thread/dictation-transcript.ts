/** `said` after what's already in the box, with one space between. */
export function joinDictated(before: string, said: string) {
  if (!before.trim()) return said
  if (!said) return before
  return /\s$/.test(before) ? before + said : `${before} ${said}`
}

/**
 * Everything heard since the mic was tapped. The recognizer reports the words of its current segment
 * (each result replaces the last); a pause, a recognizer that starts its transcript over, or a fresh
 * recognition after one ends begins a new segment, and the earlier ones are kept.
 */
export class DictationTranscript {
  /** The finished segments. */
  private done = ''
  /** The running segment's words so far. */
  private current = ''
  /** The segment just closed by `ends`: a result that repeats it is the same words, not new ones. */
  private closed: string | null = null

  get text() {
    return joinDictated(this.done, this.current)
  }

  /** A result: the running segment's words; `ends` when the recognizer says the segment is over (a final, a pause). */
  heard(said: string, ends: boolean) {
    let words = said
    if (this.closed !== null) {
      if (said.startsWith(this.closed)) words = said.slice(this.closed.length).trimStart()
      else this.closed = null
    }
    if (this.current && startsOver(this.current, words)) this.done = joinDictated(this.done, this.current)
    this.current = words
    if (ends) {
      this.done = joinDictated(this.done, this.current)
      this.current = ''
      this.closed = said
    }
  }

  /** The recognition ended without a final; a fresh one starts with nothing heard. */
  restart() {
    this.done = joinDictated(this.done, this.current)
    this.current = ''
    this.closed = null
  }
}

/**
 * The recognizer started a new segment without saying so: the result is shorter than the last one and
 * begins with another letter. A correction of the same words keeps its start or grows.
 */
function startsOver(previous: string, said: string) {
  if (!said || said.length >= previous.length) return false
  return said[0]!.toLowerCase() !== previous[0]!.toLowerCase()
}
