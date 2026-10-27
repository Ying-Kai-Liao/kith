import Foundation

// The Kith cloud's types (`CloudComputer`, `CloudDevice`, `CloudGrant`, `AccessStatus`, …), paths and error codes come
// from its contract, serve/contract/cloud/contract.json, through CloudContract.gen.swift. What is here is this app's own:
// the relay mailbox, and what a cloud type means to it. Times are epoch milliseconds.

extension CloudComputer: Identifiable {
    public var id: ComputerID { computerId }
    /// Invited to it rather than its owner. A role this version doesn't know gets the least (member); none (a cloud
    /// from before invites) is the owner.
    public var isMember: Bool { role != nil && role != .owner }
    public var isOnline: Bool { online ?? false }
}

extension CloudDevice: Identifiable {
    public var id: String { deviceId }
}

extension CloudGrant: Identifiable {
    public var id: String { grantId }
}

/// A started access request, after both nonces were exchanged: `code` is ready to show.
public struct AccessTicket: Codable, Hashable, Sendable, Identifiable {
    public var requestId: String
    public var expiresAt: Int64
    /// The 6-digit SAS to compare with the one the computer shows.
    public var code: String
    public var computerId: ComputerID
    /// The host key the SAS was computed with; pinned once the request is approved.
    public var signKey: String
    /// The label family the SAS was computed under; the pairing keeps it. nil = a ticket from before the Kith rename.
    public var labels: LabelFamily?
    public var id: String { requestId }

    public init(requestId: String, expiresAt: Int64, code: String, computerId: ComputerID, signKey: String, labels: LabelFamily? = nil) {
        self.requestId = requestId
        self.expiresAt = expiresAt
        self.code = code
        self.computerId = computerId
        self.signKey = signKey
        self.labels = labels
    }
}

/// A message waiting in the relay mailbox for a computer that is offline.
public struct QueuedItem: Codable, Hashable, Sendable, Identifiable {
    public var nonce: String
    public var exp: Int64?
    /// `queued`, or `delivering` (handed to the computer, not acknowledged yet).
    public var state: String
    public var id: String { nonce }
}

public enum MailboxEvent: Sendable, Equatable {
    case delivered(nonce: String)
    case failed(nonce: String, code: String)
    case expired(nonce: String)

    public var nonce: String {
        switch self {
        case let .delivered(nonce), let .failed(nonce, _), let .expired(nonce): nonce
        }
    }
}

public enum MailboxCancel: Sendable, Equatable {
    /// Removed before the computer saw it.
    case cancelled
    /// Already handed to the computer: it will run.
    case delivering
    /// The relay doesn't have it (delivered, expired, or not reachable right now).
    case unknown
}

/// Why the relay refused a mailbox message.
public enum MailboxError: LocalizedError, Sendable, Equatable {
    /// The computer came online: send over the channel instead.
    case hostOnline
    case rejected(String)

    public var errorDescription: String? {
        switch self {
        case .hostOnline: "Your computer just came online. Send again."
        case .rejected("full"): "Too many messages are waiting for this computer."
        case .rejected("tooLarge"): "This message is too long to hold while the computer is offline."
        case let .rejected(code): "Couldn't hold the message for your computer (\(code))."
        }
    }
}
