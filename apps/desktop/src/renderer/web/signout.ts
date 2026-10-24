import type { Signer } from '../../shared/relay-core/crypto.ts'

/**
 * Signing out on purpose (design §4.2, the order the model checked):
 *  1. abort every in-flight cloud request and close the sockets, so a register or connect signed before this cannot land after it;
 *  2. a signed `POST /v1/devices/retire`: the cloud revokes the row, or tombstones a key it never saw;
 *  3. delete the key and the kept invite tokens;
 *  4. end the Clerk session.
 * Steps 3 and 4 run even when 2 fails (offline): the ended session refuses admission and the deleted key signs nothing new.
 */
export interface SignOutSteps {
  halt(): void
  closeLinks(): void
  /** The existing key; null when this browser never made one (nothing to retire). */
  signer(): Promise<Signer | null>
  retire(signer: Signer): Promise<{ ok: boolean }>
  wipe(): Promise<void>
  endSession(): Promise<void>
}

export interface SignOutResult {
  /** The cloud confirmed the retirement. False when offline or when there was no key. */
  retired: boolean
}

export async function signOutInOrder(steps: SignOutSteps): Promise<SignOutResult> {
  steps.halt()
  steps.closeLinks()
  let retired = false
  try {
    const signer = await steps.signer()
    if (signer) retired = (await steps.retire(signer)).ok
  } catch {
    // Offline or refused: the remaining steps still cut this browser off.
  }
  try {
    await steps.wipe()
  } finally {
    await steps.endSession()
  }
  return { retired }
}
