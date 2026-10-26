import type { InviteAccepted } from './cloud-contract.gen.ts'
import { inviteProof, type InviteLink } from './invite.ts'

/** The invitee's side of a link, as steps the app can show. Every dependency is injected so the order is testable. */

export type JoinStep = 'signingIn' | 'joining' | 'asking' | 'connecting' | 'proving' | 'done' | 'waiting' | 'failed'

export interface JoinState {
  step: JoinStep
  /** The words to show for `failed` and `waiting`. */
  message: string | null
  /** The Mac the invite is for, once known. */
  computerId: string | null
}

export interface CloudFailureLike {
  status: number
  message: string | null
}

/** A call to the owner's Mac over the end-to-end connection. */
export type HostCaller = (method: string, body: unknown) => Promise<unknown>

export interface JoinDeps {
  signedIn(): boolean
  /** Resolves once the account is signed in (the person picks Apple, Google or GitHub on the sheet). */
  waitForSignIn(): Promise<void>
  /** POST /v1/invites/<id>/accept with `{token: h1}`. Throws `CloudFailureLike`. */
  accept(inviteId: string, h1: string): Promise<InviteAccepted>
  /** POST /v1/computers/<c>/access-requests; asking again for the same Mac must be harmless. */
  requestAccess(computerId: string): Promise<void>
  /** The connection to that Mac, or null while it is offline. Throws when this app can't connect at all. */
  connect(computerId: string): Promise<HostCaller | null>
  /** Kept per computer in the secure store, so a reconnect can prove again. */
  keepToken(computerId: string, inviteId: string, token: string): Promise<void>
}

const state = (step: JoinStep, computerId: string | null = null, message: string | null = null): JoinState => ({ step, message, computerId })
const failed = (message: string, computerId: string | null = null) => state('failed', computerId, message)

export const messageOf = (e: unknown, fallback: string) =>
  e instanceof Error && e.message
    ? e.message
    : e && typeof e === 'object' && 'message' in e && (e as { message: unknown }).message
      ? String((e as { message: unknown }).message)
      : fallback

/** The cloud's own words for 403, 404, 409 and 410; anything else gets a plain line. */
const acceptText = (e: CloudFailureLike) => e.message ?? (e.status === 0 ? "Can't reach the Kith cloud. Check your connection." : "Couldn't join. Try the link again.")

/** The host says this device already showed the invite: a success on a repeat. */
const alreadyProven = (m: string) => /already shown its invite/i.test(m)

/** A link failure, not the Mac's answer: the same call can work once it is reachable again. */
const isTransient = (e: unknown) => (e as { status?: number } | null)?.status === 0

/**
 * Runs the whole join. Safe to run again after `waiting` (the Mac was offline): accept answers the same
 * membership for the same account, and the token is proven again.
 */
export async function runJoin(link: InviteLink, deps: JoinDeps, report: (s: JoinState) => void): Promise<JoinState> {
  const end = (s: JoinState) => (report(s), s)
  if (!deps.signedIn()) {
    report(state('signingIn'))
    try {
      await deps.waitForSignIn()
    } catch (e) {
      return end(failed(messageOf(e, 'Sign-in was cancelled.')))
    }
  }

  report(state('joining', link.computerId))
  const computerId = link.computerId
  try {
    // Only h1 leaves for the cloud; the token itself stays here and goes to the Mac.
    const joined = await deps.accept(link.inviteId, await inviteProof(link.token))
    // The link's `c` is the only computer this token may go to: the cloud's answer can't steer it elsewhere.
    if (joined.computerId && joined.computerId !== link.computerId) {
      return end(failed("This invite doesn't match the computer in its link. Ask for a new one.", link.computerId))
    }
  } catch (e) {
    return end(failed(acceptText(e as CloudFailureLike), link.computerId))
  }
  try {
    await deps.keepToken(computerId, link.inviteId, link.token)
  } catch (e) {
    // Never carry on without a safe place for the token: a reconnect couldn't prove again.
    return end(failed(messageOf(e, "Can't store the invite safely on this computer."), computerId))
  }

  report(state('asking', computerId))
  try {
    await deps.requestAccess(computerId)
  } catch (e) {
    return end(failed(messageOf(e, "Couldn't ask the Mac for access."), computerId))
  }

  report(state('connecting', computerId))
  let call: HostCaller | null
  try {
    call = await deps.connect(computerId)
  } catch (e) {
    return end(failed(messageOf(e, "Couldn't connect to the Mac."), computerId))
  }
  if (!call) return end(state('waiting', computerId, "The Mac isn't reachable. This finishes when it is back online."))

  report(state('proving', computerId))
  try {
    // The first call to the Mac; until it succeeds the Mac refuses everything else.
    await call('proveInvite', { inviteId: link.inviteId, token: link.token })
  } catch (e) {
    const m = messageOf(e, "The Mac didn't accept this invite.")
    if (!alreadyProven(m)) {
      return end(isTransient(e) ? state('waiting', computerId, 'Lost the connection to the Mac. This finishes when it is back.') : failed(m, computerId))
    }
  }
  return end(state('done', computerId))
}
