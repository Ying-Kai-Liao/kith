import Foundation

// Access and Key cards (host `cards/secret.rs`, kernel PROTOCOL "Keys"): the agent needs a service's key. The owner
// picks an account or types the key into a secure field. These types carry names and states only, never a value.

/// `data.secret` of a card with `data.card` `account` or `key`.
public struct SecretRequest: Codable, Hashable, Sendable {
    public var service: String
    /// The variables the service's tool reads (`GITHUB_TOKEN`); one secure field each.
    public var vars: [String]
    /// The key's profile (`github/work`) once known: fixed on a replace, set after it saved.
    public var profile: String?
    public var account: String?
    public var purpose: String?
    /// The key exists and is being replaced: the profile stays, no account label to ask.
    public var replaces: Bool
    /// `pending` | `saved` | `denied` | `expired`
    public var state: String

    public init(service: String, vars: [String], profile: String? = nil, account: String? = nil, purpose: String? = nil,
                replaces: Bool = false, state: String = "pending") {
        self.service = service
        self.vars = vars
        self.profile = profile
        self.account = account
        self.purpose = purpose
        self.replaces = replaces
        self.state = state
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        service = try c.decode(String.self, forKey: .service)
        vars = try c.decodeIfPresent([String].self, forKey: .vars) ?? []
        profile = try c.decodeIfPresent(String.self, forKey: .profile)
        account = try c.decodeIfPresent(String.self, forKey: .account)
        purpose = try c.decodeIfPresent(String.self, forKey: .purpose)
        replaces = try c.decodeIfPresent(Bool.self, forKey: .replaces) ?? false
        state = try c.decodeIfPresent(String.self, forKey: .state) ?? "pending"
    }

    public var pending: Bool { state == "pending" }
}

/// One of the owner's accounts for a service, and the agents that hold it.
public struct SecretAccount: Codable, Hashable, Sendable, Identifiable {
    public var profile: String
    public var account: String
    public var agents: [String]
    public var id: String { profile }

    public init(profile: String, account: String, agents: [String] = []) {
        self.profile = profile
        self.account = account
        self.agents = agents
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        profile = try c.decode(String.self, forKey: .profile)
        account = try c.decodeIfPresent(String.self, forKey: .account) ?? profile.split(separator: "/").last.map(String.init) ?? profile
        agents = try c.decodeIfPresent([String].self, forKey: .agents) ?? []
    }
}

/// What `provideSecret` returns: names only.
public struct SecretSaved: Decodable, Hashable, Sendable {
    public var profile: String
    public var service: String
    public var vars: [String]
}

/// One saved key in Settings › Keys. Never a value.
public struct KeyRecord: Codable, Hashable, Sendable, Identifiable {
    public var profile: String
    public var service: String
    public var account: String
    public var owner: String?
    public var vars: [String]
    public var agents: [String]
    /// ISO 8601, or nil when no agent has used it.
    public var lastUsed: String?
    public var id: String { profile }

    public init(profile: String, service: String, account: String, owner: String? = nil, vars: [String] = [], agents: [String] = [],
                lastUsed: String? = nil) {
        self.profile = profile
        self.service = service
        self.account = account
        self.owner = owner
        self.vars = vars
        self.agents = agents
        self.lastUsed = lastUsed
    }

    private enum CodingKeys: String, CodingKey { case profile, service, account, owner, vars, agents, lastUsed = "last_used" }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        profile = try c.decode(String.self, forKey: .profile)
        service = try c.decodeIfPresent(String.self, forKey: .service) ?? profile.split(separator: "/").first.map(String.init) ?? profile
        account = try c.decodeIfPresent(String.self, forKey: .account) ?? profile.split(separator: "/").last.map(String.init) ?? profile
        owner = try c.decodeIfPresent(String.self, forKey: .owner)
        vars = try c.decodeIfPresent([String].self, forKey: .vars) ?? []
        agents = try c.decodeIfPresent([String].self, forKey: .agents) ?? []
        lastUsed = try c.decodeIfPresent(String.self, forKey: .lastUsed)
    }

    public var lastUsedDate: Date? {
        guard let lastUsed else { return nil }
        let plain = ISO8601DateFormatter()
        if let date = plain.date(from: lastUsed) { return date }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: lastUsed)
    }

    /// The keys grouped by service, services and accounts in name order.
    public static func grouped(_ keys: [KeyRecord]) -> [(service: String, keys: [KeyRecord])] {
        Dictionary(grouping: keys, by: \.service).map { (service: $0.key, keys: $0.value.sorted { $0.account < $1.account }) }
            .sorted { $0.service < $1.service }
    }
}

/// The rules a Key card checks before it sends anything.
public enum SecretRules {
    /// The host's account label rule: `^[a-z0-9][a-z0-9._-]{0,39}$`.
    public static func validAccount(_ name: String) -> Bool {
        guard (1...40).contains(name.utf8.count), let first = name.unicodeScalars.first else { return false }
        func lower(_ u: Unicode.Scalar) -> Bool { ("a"..."z").contains(u) || ("0"..."9").contains(u) }
        guard lower(first) else { return false }
        return name.unicodeScalars.allSatisfy { lower($0) || $0 == "." || $0 == "_" || $0 == "-" }
    }

    /// 1...1536 bytes, no NUL.
    public static func validValue(_ value: String) -> Bool {
        (1...1536).contains(value.utf8.count) && !value.utf8.contains(0)
    }

    /// The Access card's option id for an account.
    public static func optionId(profile: String) -> String { "profile:\(profile)" }
}

public extension EntryData {
    /// An Access or Key card: the app lays it out itself, and no surface answers it by voice or shows a field for it.
    var isSecretCard: Bool { card == "account" || card == "key" }
}
