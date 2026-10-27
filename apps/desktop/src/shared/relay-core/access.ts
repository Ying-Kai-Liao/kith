import { paths, receive, type Requests } from '../cloud-contract.gen.ts'
import type { AccessTicket, CloudResult } from '../relay.ts'
import { b64url, computerIdOf, randomBytes, sasCode, sasCommit, unb64url, type Signer } from './crypto.ts'

/**
 * Asking a Mac for this device's access (§4.2 B, the commit-then-reveal SAS): commit to a device nonce, wait for the
 * Mac's nonce, reveal ours, and work out the six digits the owner checks on the Mac. The device nonce lives only in this
 * function's memory, so the cloud can't learn it before the Mac has committed to its own. Port of CloudClient.requestAccess.
 */

export type CloudRequest = (method: string, path: string, body: unknown, signed: boolean) => Promise<CloudResult<unknown>>

const failure = (status: number, code: string, message: string): CloudResult<never> => ({ ok: false, error: { status, code, message } })

export interface AccessAsk {
  request: CloudRequest
  signer: Signer
  computerId: string
  /** The Mac's Ed25519 key, b64url; it must hash to `computerId`. */
  signKey: string
  /** Stop waiting for the Mac's nonce (the person cancelled or asked again). */
  cancelled: () => boolean
  /** The request exists on the cloud; its id is what cancelling withdraws. */
  created?: (requestId: string) => void
  pollMs?: number
}

export async function requestAccess(ask: AccessAsk): Promise<CloudResult<AccessTicket>> {
  const hostKey = unb64url(ask.signKey)
  if (!hostKey || hostKey.length !== 32 || computerIdOf(hostKey) !== ask.computerId) {
    return failure(0, 'badSignKey', "This computer's key doesn't match its ID.")
  }
  const nD = randomBytes(32)
  const commit: Requests['createAccessRequest'] = { commit: b64url(sasCommit(ask.signer.publicKey, nD)) }
  const made = await ask.request('POST', paths.createAccessRequest(ask.computerId), commit, true)
  if (!made.ok) return made
  const { requestId, expiresAt } = receive('AccessRequestCreated', made.value)
  ask.created?.(requestId)
  let hostNonce: Uint8Array | null = null
  while (!hostNonce) {
    if (ask.cancelled()) return failure(0, 'cancelled', 'The request was cancelled.')
    const status = await ask.request('GET', paths.getAccessRequest(requestId), undefined, false)
    if (!status.ok) return status
    const s = receive('AccessStatus', status.value)
    // A state this version doesn't know (or none) is over, as an expired request is.
    const over = s.status === 'expired' || s.status === 'unknown' || s.status === undefined ? 'requestExpired' : s.status
    if (s.status !== 'pending') return failure(0, over, "The computer didn't take the request.")
    if (s.hostNonce) {
      hostNonce = unb64url(s.hostNonce)
      if (!hostNonce || hostNonce.length !== 32) return failure(0, 'badNonce', 'The computer sent something unreadable.')
    } else {
      if (Date.now() >= expiresAt) return failure(410, 'requestExpired', 'The request expired. Ask again.')
      await new Promise((r) => setTimeout(r, ask.pollMs ?? 2000))
    }
  }
  const reveal: Requests['revealAccessRequest'] = { nonce: b64url(nD) }
  const revealed = await ask.request('POST', paths.revealAccessRequest(requestId), reveal, true)
  if (!revealed.ok) return revealed
  return { ok: true, value: { requestId, expiresAt, computerId: ask.computerId, code: sasCode(hostKey, ask.signer.publicKey, nD, hostNonce) } }
}
