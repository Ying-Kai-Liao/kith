import Foundation

// Wire types for inviting people (host methods `people`, `invites`, `createInvite`, `audit`;
// serve/host/src/api/people.rs) and the cloud's accept answer. Times are epoch milliseconds.
// Decoding is lenient: unknown fields are ignored and a role this version doesn't know is a guest.

/// What an invited person may do with one agent.
public enum InviteRole: String, Codable, CaseIterable, Sendable {
    case member, guest

    public init(from decoder: Decoder) throws {
        self = InviteRole(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .guest
    }

    public var title: String { self == .member ? "Member" : "Guest" }
}

public struct HostPersonDevice: Codable, Hashable, Sendable, Identifiable {
    public var key: String
    public var name: String?
    public var platform: String?
    public var lastSeenAt: Int64?
    public var connected: Bool?
    public var id: String { key }

    public var lastSeen: Date? { lastSeenAt.map { Date(timeIntervalSince1970: Double($0) / 1000) } }
}

/// One invited person, from `people`.
public struct HostPerson: Codable, Hashable, Sendable, Identifiable {
    public var personId: String
    public var name: String
    public var agents: [String: InviteRole]
    public var projects: [String]
    public var joined: Bool
    public var openInvites: Int
    public var devices: [HostPersonDevice]
    /// Their bots join without waiting for the owner (`setAutoAllowBots`). False from an older host.
    public var autoBots: Bool
    public var id: String { personId }

    public init(personId: String, name: String, agents: [String: InviteRole] = [:], projects: [String] = [],
                joined: Bool = false, openInvites: Int = 0, devices: [HostPersonDevice] = [], autoBots: Bool = false) {
        self.personId = personId
        self.name = name
        self.agents = agents
        self.projects = projects
        self.joined = joined
        self.openInvites = openInvites
        self.devices = devices
        self.autoBots = autoBots
    }

    private enum CodingKeys: String, CodingKey { case personId, name, agents, projects, joined, openInvites, devices, autoBots }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        personId = try c.decode(String.self, forKey: .personId)
        let name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        self.name = name.isEmpty ? personId : name
        agents = try c.decodeIfPresent([String: InviteRole].self, forKey: .agents) ?? [:]
        projects = try c.decodeIfPresent([String].self, forKey: .projects) ?? []
        joined = try c.decodeIfPresent(Bool.self, forKey: .joined) ?? false
        openInvites = try c.decodeIfPresent(Int.self, forKey: .openInvites) ?? 0
        devices = try c.decodeIfPresent([HostPersonDevice].self, forKey: .devices) ?? []
        autoBots = try c.decodeIfPresent(Bool.self, forKey: .autoBots) ?? false
    }

    /// The most recent time any of their devices was seen.
    public var lastSeen: Date? { devices.compactMap(\.lastSeen).max() }
}

/// One invite, from `invites`.
public struct HostInvite: Codable, Hashable, Sendable, Identifiable {
    public var inviteId: String
    public var personId: String
    public var agents: [String: InviteRole]
    public var createdAt: Int64
    public var expiresAt: Int64
    public var usedAt: Int64?
    public var revokedAt: Int64?
    public var id: String { inviteId }

    public var expiry: Date { Date(timeIntervalSince1970: Double(expiresAt) / 1000) }

    /// Still waiting for the person: not used, not cancelled and not expired.
    public func isOpen(at now: Date = .now) -> Bool {
        usedAt == nil && revokedAt == nil && expiry > now
    }
}

/// What `createInvite` returns.
public struct CreatedInvite: Codable, Hashable, Sendable {
    public var inviteId: String
    public var link: String
    public var expiresAt: Int64
    public var personId: String?

    public var expiry: Date { Date(timeIntervalSince1970: Double(expiresAt) / 1000) }
}

/// One request a non-owner made, from `audit`.
public struct AuditRow: Codable, Hashable, Sendable, Identifiable {
    public var id: Int64
    public var at: Int64
    public var personId: String
    public var method: String
    public var target: String?
    public var allowed: Bool

    public var time: Date { Date(timeIntervalSince1970: Double(at) / 1000) }
}

/// A member's new device asking to use this computer (`accessRequests`), as the owner's phone sees it: the computer
/// lists only members' requests to its other devices, and only those may be answered from here.
public struct MemberAccessRequest: Codable, Hashable, Sendable, Identifiable {
    public var requestId: String
    public var deviceName: String
    public var platform: String
    public var email: String?
    public var personId: String?
    /// The six digits the device shows, once it revealed them; Approve waits for it.
    public var code: String?
    public var expiresAt: Int64
    public var id: String { requestId }

    public init(requestId: String, deviceName: String, platform: String, email: String? = nil, personId: String? = nil,
                code: String? = nil, expiresAt: Int64) {
        self.requestId = requestId
        self.deviceName = deviceName
        self.platform = platform
        self.email = email
        self.personId = personId
        self.code = code
        self.expiresAt = expiresAt
    }

    /// "123 456", as the device shows it.
    public var spacedCode: String? {
        code.map { $0.count == 6 ? "\($0.prefix(3)) \($0.suffix(3))" : $0 }
    }
}
