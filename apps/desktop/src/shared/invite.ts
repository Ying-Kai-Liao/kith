/** An invite link, `kith://invite?c=<computerId>&i=<inviteId>&t=<token>`, and the proof the cloud gets instead of the token. */

export interface InviteLink {
  computerId: string
  inviteId: string
  /** The secret from the link. It goes to the owner's Mac only, never to the cloud and never into a log. */
  token: string
}

const ID = /^[A-Za-z0-9_-]{1,128}$/
const TOKEN = /^[A-Za-z0-9_-]{8,512}$/

/** The link's three parts, or null when the URL isn't an invite or a part is missing or odd. */
export function parseInviteLink(url: string, schemes: readonly string[]): InviteLink | null {
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return null
  }
  const scheme = parsed.protocol.replace(/:$/, '').toLowerCase()
  // A pasted page link: https://<host>/i/<computerId>/<inviteId>#t=<token> (the token stays in the fragment).
  if (scheme === 'https') {
    const m = /^\/i\/([^/]+)\/([^/]+)\/?$/.exec(parsed.pathname)
    const t = new URLSearchParams(parsed.hash.replace(/^#/, '')).get('t')
    if (!m || !t || !ID.test(m[1]) || !ID.test(m[2]) || !TOKEN.test(t)) return null
    return { computerId: m[1], inviteId: m[2], token: t }
  }
  if (!schemes.some((s) => s.toLowerCase() === scheme)) return null
  // `kith://invite?...` reads as host "invite"; some platforms deliver `kith:invite?...` or `kith:///invite`.
  const where = (parsed.hostname || parsed.pathname.replace(/^\/+/, '')).toLowerCase().replace(/\/+$/, '')
  if (where !== 'invite') return null
  const c = parsed.searchParams.get('c')
  const i = parsed.searchParams.get('i')
  const t = parsed.searchParams.get('t')
  if (!c || !i || !t || !ID.test(c) || !ID.test(i) || !TOKEN.test(t)) return null
  return { computerId: c, inviteId: i, token: t }
}

/** h1: lowercase hex of sha256 of the token's UTF-8 bytes. The cloud only ever sees this. */
export async function inviteProof(token: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(token))
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('')
}

export const isInviteProof = (s: string) => /^[0-9a-f]{64}$/.test(s)
