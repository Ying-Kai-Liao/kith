import type { InviteLink } from './invite.ts'

/** What an invite is for, shown before anyone joins: who sent it, which computer, and which agents and rooms it opens. */

export interface InvitePreviewItem {
  kind: 'agent' | 'room'
  id: string
  name: string
  /** One plain line on what it is good for. */
  suitableFor: string
}

export interface InvitePreview {
  inviterName: string
  computerName: string
  note?: string
  items: InvitePreviewItem[]
}

export interface InvitePreviewProvider {
  preview(link: InviteLink): Promise<InvitePreview>
}

/**
 * Stands in until the cloud can answer this. The real cloud call replaces it and must keep the token on the device: only
 * the computer and invite ids may leave. The iOS app uses the same JSON shape.
 */
export const mockInvitePreviewProvider: InvitePreviewProvider = {
  async preview(link) {
    // Deterministic per invite: the same link always shows the same thing.
    return {
      inviterName: 'Kai',
      computerName: "Kai's Mac mini",
      note: `Invite ${link.inviteId.slice(0, 4)}: come and help with the household.`,
      items: [
        { kind: 'agent', id: 'finance', name: 'Finance', suitableFor: 'Budgets, receipts and monthly spending questions' },
        { kind: 'room', id: 'family', name: 'Family', suitableFor: 'Plans and shared lists with the family' },
      ],
    }
  },
}
