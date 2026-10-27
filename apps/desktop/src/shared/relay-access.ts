import type { HostCaller, JoinDeps } from './invite-join.ts'
import type { AccessState } from './cloud-contract.gen.ts'
import type { AccessProgress, AccessTicket, CloudResult, RelayComputer, RelayState } from './relay.ts'

/** The part of `window.kith.relay` that asking a Mac for access and talking to it needs; injected so the order is testable. */
export interface RelayApi {
  computers(): Promise<CloudResult<RelayComputer[]>>
  requestAccess(computerId: string, signKey: string): Promise<CloudResult<AccessTicket>>
  accessStatus(requestId: string): Promise<CloudResult<AccessProgress>>
  cancelAccess(computerId: string, requestId: string | null): Promise<void>
  state(computerId: string, signKey: string): Promise<RelayState>
  call(computerId: string, signKey: string, method: string, body: unknown, timeoutMs: number): Promise<unknown>
}

/** How asking ended: every access-request state but pending (AccessState in the cloud contract). */
export type AccessOutcome = Exclude<AccessState, 'pending'>

export interface WaitOptions {
  /** The six digits are ready: the device shows them while it waits (in Code mode the Mac's owner compares them, a member's too). */
  onCode?: (ticket: AccessTicket) => void
  cancelled?: () => boolean
  intervalMs?: number
  sleep?: (ms: number) => Promise<void>
  now?: () => number
}

const nap = (ms: number) => new Promise<void>((r) => setTimeout(r, ms))

/** Asks a Mac for this device's access and waits for the answer. Throws the cloud's words when asking itself fails. */
export async function askForAccess(api: RelayApi, c: { computerId: string; signKey: string }, o: WaitOptions = {}): Promise<AccessOutcome> {
  const res = await api.requestAccess(c.computerId, c.signKey)
  // The cloud's 409 here is "This device already has access": the answer asking would wait for.
  if (!res.ok && res.error.status === 409) return 'approved'
  if (!res.ok) throw new Error(res.error.message ?? "Couldn't ask the Mac for access.")
  const ticket = res.value
  o.onCode?.(ticket)
  const sleep = o.sleep ?? nap
  const now = o.now ?? Date.now
  for (;;) {
    if (o.cancelled?.()) {
      await api.cancelAccess(c.computerId, ticket.requestId)
      return 'cancelled'
    }
    if (now() >= ticket.expiresAt) return 'expired'
    const status = await api.accessStatus(ticket.requestId)
    if (status.ok) {
      const state = status.value
      // A state this version doesn't know is over as far as it can tell (as on iOS): never read as approved.
      if (state === 'unknown') return 'expired'
      if (state !== 'pending') return state
    }
    await sleep(o.intervalMs ?? 2000)
  }
}

// One line per outcome: a state added to the contract doesn't compile here until it has its words.
const ACCESS_TEXT: Record<AccessOutcome, string> = {
  approved: '',
  denied: 'The Mac said no.',
  expired: 'The request ran out of time. Try again.',
  cancelled: 'Cancelled.',
}

export const accessText = (o: AccessOutcome) => ACCESS_TEXT[o]

async function find(api: RelayApi, computerId: string): Promise<RelayComputer> {
  const list = await api.computers()
  if (!list.ok) throw new Error(list.error.message ?? "Can't reach the Kith cloud. Check your connection.")
  const c = list.value.find((x) => x.computerId === computerId)
  if (!c) throw new Error("This Mac isn't in your account yet.")
  return c
}

/** A refusal this long after asking came from the Mac's owner, not from a Mac that hadn't heard of the member yet. */
const OWNER_ANSWER_MS = 15_000

/** The invitee's side of a join over the relay: the cloud accept is the caller's; this asks the Mac and reaches it. */
export function relayJoinDeps(
  api: RelayApi,
  base: Pick<JoinDeps, 'signedIn' | 'waitForSignIn' | 'accept' | 'keepToken'>,
  wait: WaitOptions = {},
): JoinDeps {
  return {
    ...base,
    async requestAccess(computerId, onCode) {
      const c = await find(api, computerId)
      if (c.access === 'granted') return
      // Right after the cloud accept the Mac may not have heard of the new member yet and turns the request down:
      // asking again a few seconds later is the same request done properly. That refusal comes at once; a later
      // one is the owner saying no after comparing the code, and is final.
      const sleep = wait.sleep ?? nap
      const now = wait.now ?? Date.now
      for (let attempt = 1; ; attempt++) {
        let outcome: AccessOutcome | null = null
        let failure: Error | null = null
        const asked = now()
        try {
          outcome = await askForAccess(api, c, { ...wait, onCode: (t) => (wait.onCode?.(t), onCode?.(t.code)) })
        } catch (e) {
          failure = e instanceof Error ? e : new Error(String(e))
        }
        if (outcome === 'approved') return
        const turnedDown = outcome === 'denied' || (failure !== null && /didn't take/i.test(failure.message))
        const byOwner = now() - asked >= OWNER_ANSWER_MS
        if (!turnedDown || byOwner || attempt >= 4 || wait.cancelled?.()) throw failure ?? new Error(accessText(outcome!))
        await sleep(4000)
      }
    },
    async connect(computerId): Promise<HostCaller | null> {
      const c = await find(api, computerId)
      if (!c.online) return null
      const state = await api.state(c.computerId, c.signKey)
      if (state.kind === 'hostOffline') return null
      if (state.kind === 'failed' && state.final) throw new Error(state.message)
      if (state.kind === 'unauthorized' && !state.retrying) throw new Error(state.message)
      return (method, body) => api.call(c.computerId, c.signKey, method, body, 30_000)
    },
  }
}
