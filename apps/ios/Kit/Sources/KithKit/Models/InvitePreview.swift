import Foundation

/// What an invite is for, shown to the invitee before they sign in: who invited them, to which computer, and which
/// agents and rooms they get, each with what it is good for.
public struct InvitePreview: Codable, Equatable, Sendable {
    public var inviterName: String
    public var computerName: String
    public var note: String?
    public var items: [InvitePreviewItem]

    public init(inviterName: String, computerName: String, note: String? = nil, items: [InvitePreviewItem]) {
        self.inviterName = inviterName
        self.computerName = computerName
        self.note = note
        self.items = items
    }
}

public struct InvitePreviewItem: Codable, Equatable, Sendable, Identifiable {
    public enum Kind: String, Codable, Sendable {
        case agent, room
    }

    public var kind: Kind
    public var id: String
    public var name: String
    /// One line on what it is good for.
    public var suitableFor: String

    public init(kind: Kind, id: String, name: String, suitableFor: String) {
        self.kind = kind
        self.id = id
        self.name = name
        self.suitableFor = suitableFor
    }
}
