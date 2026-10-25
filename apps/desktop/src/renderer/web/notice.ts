import type { RelayState } from '../../shared/relay.ts'

/**
 * What the page says when the link to a Mac is not simply working (design §3 step 7, §4.5 table). Pure, so the words and
 * the action each outcome leads to are tested without a browser.
 */
export type Notice =
  | { kind: 'none' }
  | { kind: 'info'; text: string }
  /** The session is gone or refused: the person signs in again; the key and the invite stay, nothing is proven twice. */
  | { kind: 'signIn'; text: string }
  /** Access was taken away or the key is no longer valid: the person is told, and reloading starts clean. */
  | { kind: 'removed'; text: string }
  | { kind: 'error'; text: string }

/** `computer` is the name the owner gave it ("Kai's MacBook Pro"); the cloud's row has it. */
export function noticeFor(state: RelayState | null, computer: string): Notice {
  if (!state) return { kind: 'none' }
  switch (state.kind) {
    case 'connecting':
    case 'ready':
      return { kind: 'none' }
    case 'hostOffline':
      return { kind: 'info', text: `${computer || 'The computer'} is offline. Your chats come back when it is on.` }
    case 'unauthorized':
      return state.retrying
        ? { kind: 'info', text: "The computer couldn't confirm your access just now. Retrying." }
        : { kind: 'removed', text: state.message || 'Your access to this computer was removed.' }
    case 'failed':
      if (state.code === 'signedOut') return { kind: 'signIn', text: 'Your session ended. Sign in again to carry on.' }
      if (state.code === 'deviceRetired') return { kind: 'removed', text: state.message }
      if (state.code === 'unknownComputer') return { kind: 'removed', text: "This computer isn't shared with you any more." }
      if (state.code === 'upgrade') return { kind: 'error', text: state.message }
      return state.final ? { kind: 'error', text: state.message } : { kind: 'info', text: state.message }
  }
}

/** What the list says when nothing is shared with the account: owners are pointed to the app, everyone else to an invite. */
export function emptyAccountText(computers: readonly { role: string }[]): string {
  return computers.some((c) => c.role === 'owner')
    ? 'Kith on the web is for people you invite. Use the Kith app on your Mac or iPhone; to try the web, open an invite link with another account.'
    : "Open an invite link to join someone's Kith."
}
