import Foundation

/// Where the invite sheet gets its preview. A failure never blocks joining: the sheet falls back to a generic line.
public protocol InvitePreviewProvider: Sendable {
    func preview(for link: InviteLink) async throws -> InvitePreview
}

/// Stand-in data until the cloud can say what an invite is for; the real cloud call will replace it.
/// The same invite always gets the same preview.
public struct MockInvitePreviewProvider: InvitePreviewProvider {
    public init() {}

    public func preview(for link: InviteLink) async throws -> InvitePreview {
        InvitePreview(
            inviterName: "Kai",
            computerName: "Kai's Mac mini",
            note: nil,
            items: [
                InvitePreviewItem(kind: .agent, id: "finance", name: "Finance", suitableFor: "Budgets, receipts and monthly spending questions"),
                InvitePreviewItem(kind: .room, id: "family", name: "Family", suitableFor: "Plans and shared lists with the family"),
            ]
        )
    }
}
