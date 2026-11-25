import type { InviteLink } from '../../shared/invite.ts'
import type { InvitePreview, InvitePreviewItem, InvitePreviewProvider } from '../../shared/invite-preview.ts'

/** The preview, or null when the provider failed: a preview is a courtesy and never blocks the join. */
export async function loadPreview(provider: InvitePreviewProvider, link: InviteLink): Promise<InvitePreview | null> {
  try {
    return await provider.preview(link)
  } catch {
    return null
  }
}

/** The chat to open after a join: the first previewed item that matches a bot by id or by name, else null (the chat list). */
export function pickDoneTarget(items: InvitePreviewItem[], bots: { id: string; name: string }[]): string | null {
  for (const item of items) {
    const hit = bots.find((b) => b.id === item.id) ?? bots.find((b) => b.name.trim().toLowerCase() === item.name.trim().toLowerCase())
    if (hit) return hit.id
  }
  return null
}
