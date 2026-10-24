import { parseInviteLink, type InviteLink } from '../../shared/invite.ts'

/** The slice of `window.location` / `window.history` the invite link needs, so tests can pass fakes. */
export interface PageLocation {
  hash: string
  pathname: string
  search: string
}
export interface PageHistory {
  replaceState(state: unknown, unused: string, url: string): void
}

/**
 * Takes the invite out of the address bar (design §3 step 1): the link is `/join#c=<computerId>&i=<inviteId>&t=<token>`,
 * the secret in the fragment so no server sees it. Whatever the fragment held, it is dropped from the URL (and so from
 * history entries and the clipboard of the next copy) before anything else runs. A link with a missing or odd part gives
 * null: the page then shows the plain sign-in, never half a join.
 */
export function takeInviteFromHash(location: PageLocation, history: PageHistory): InviteLink | null {
  const raw = location.hash.replace(/^#/, '')
  if (!raw) return null
  const params = new URLSearchParams(raw)
  // Only a fragment that looks like an invite is ours to remove.
  if (!params.has('c') && !params.has('i') && !params.has('t')) return null
  history.replaceState(null, '', `${location.pathname}${location.search}`)
  const query = new URLSearchParams()
  for (const k of ['c', 'i', 't']) {
    const v = params.get(k)
    if (v !== null) query.set(k, v)
  }
  return parseInviteLink(`kith://invite?${query}`, ['kith'])
}
