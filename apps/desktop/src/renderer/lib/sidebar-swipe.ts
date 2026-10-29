type CrewBoard = 'crew' | 'board'

/** How far (points) a two-finger swipe must travel sideways before it switches the list. */
export const SWIPE_THRESHOLD = 60
/** A gesture ends after this long without a wheel event; trackpad momentum keeps events coming until then. */
export const SWIPE_QUIET_MS = 180

/**
 * Turns trackpad wheel deltas into at most one Crew/Board switch per gesture. Swiping left (deltaX > 0) goes to
 * Board, right to Crew. Mostly vertical movement is the list scrolling and never counts.
 */
export function swipeTracker() {
  let sum = 0
  let last = -Infinity
  let fired = false
  return (dx: number, dy: number, now: number): CrewBoard | null => {
    if (now - last > SWIPE_QUIET_MS) {
      sum = 0
      fired = false
    }
    last = now
    if (fired) return null
    if (Math.abs(dx) <= Math.abs(dy)) {
      sum = 0
      return null
    }
    sum += dx
    if (Math.abs(sum) < SWIPE_THRESHOLD) return null
    fired = true
    return sum > 0 ? 'board' : 'crew'
  }
}
