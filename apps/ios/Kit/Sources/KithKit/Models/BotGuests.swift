import Foundation

// Wire types for bot guests: bots from outside (Grok and others) that ask an owner's agents
// (serve/host/src/api/botguests.rs `view()`, docs/kith-bot-guests.md). Times are epoch milliseconds. Decoding is
// lenient: unknown fields are ignored, a state this version doesn't know is `.unknown`, and the nullable fields stay nil.

/// Where a bot is in its life. A state this version doesn't know is `.unknown`: a row with no actions.
public enum BotGuestState: Hashable, Sendable {
    case invited, joining, pending, active, denied, revoked, unknown(String)

    public init(raw: String) {
        switch raw {
        case "invited": self = .invited
        case "joining": self = .joining
        case "pending": self = .pending
        case "active": self = .active
        case "denied": self = .denied
        case "revoked": self = .revoked
        default: self = .unknown(raw)
        }
    }

    /// Denied and removed bots fold into "Ended".
    public var isEnded: Bool { self == .denied || self == .revoked }
}

extension BotGuestState: Codable {
    public init(from decoder: Decoder) throws {
        self.init(raw: try decoder.singleValueContainer().decode(String.self))
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .invited: try c.encode("invited")
        case .joining: try c.encode("joining")
        case .pending: try c.encode("pending")
        case .active: try c.encode("active")
        case .denied: try c.encode("denied")
        case .revoked: try c.encode("revoked")
        case let .unknown(raw): try c.encode(raw)
        }
    }
}

public struct BotGuestLimits: Codable, Hashable, Sendable {
    public var ratePerHour: Int
    public var concurrent: Int

    public init(ratePerHour: Int, concurrent: Int) {
        self.ratePerHour = ratePerHour
        self.concurrent = concurrent
    }
}

/// Which bot a chat belongs to: the `botGuest` on the chat row (serve/host/src/hub/sync.rs `bot_json`).
public struct BotGuestInfo: Codable, Hashable, Sendable {
    public var botId: String
    public var name: String
    public var provider: String
    /// The invited person who made the bot; nil for the computer's owner's own.
    public var ownerPerson: String?
    public var ownerName: String?

    public init(botId: String, name: String, provider: String = "grok", ownerPerson: String? = nil, ownerName: String? = nil) {
        self.botId = botId
        self.name = name
        self.provider = provider
        self.ownerPerson = ownerPerson
        self.ownerName = ownerName
    }

    /// "from Grok · Sam's Rex", or "from Grok · Your Rex" for the computer's owner's own bot.
    public var tag: String {
        let whose = ownerPerson == nil ? "Your" : "\(ownerName?.isEmpty == false ? ownerName! : "Someone")'s"
        return "from \(BotGuest.title(provider: provider)) · \(whose) \(name)"
    }

    /// "Rex asks from Grok. You can read along here."
    public var readOnlyLine: String { "\(name) asks from \(BotGuest.title(provider: provider)). You can read along here." }
}

extension Bot {
    /// A bot from outside's chat: read along only. No composer, no settings, no new session, no delete.
    public var isReadOnly: Bool { botGuest != nil }
}

extension BotGuestInfo {
    /// Whether this person may answer the chat's permission cards: only the computer's owner (a member owner of the bot
    /// is not a member of the agent, so the host would refuse).
    public func canAnswer(isOwner: Bool) -> Bool { isOwner }
}

/// A chat a bot started with one of the owner's agents; only the computer's owner is sent these.
public struct BotGuestConversation: Codable, Hashable, Sendable, Identifiable {
    public var conversation: String
    public var agent: String
    public var chatBotId: String?
    public var threadRoot: String?
    public var originLabel: String?
    public var lastAt: Int64?
    public var id: String { conversation }

    public init(conversation: String, agent: String, chatBotId: String? = nil, threadRoot: String? = nil,
                originLabel: String? = nil, lastAt: Int64? = nil) {
        self.conversation = conversation
        self.agent = agent
        self.chatBotId = chatBotId
        self.threadRoot = threadRoot
        self.originLabel = originLabel
        self.lastAt = lastAt
    }

    public var last: Date? { lastAt.map { Date(milliseconds: $0) } }
}

/// One bot guest, from `bots` (the computer's owner: every bot, with `personId` and `conversations`) or `myBots`.
public struct BotGuest: Codable, Hashable, Sendable, Identifiable {
    public var botId: String
    public var name: String
    /// "grok", "other", or one the host names that this version doesn't.
    public var provider: String
    public var state: BotGuestState
    /// The invited person who made it; nil for the computer's owner's own bot.
    public var ownerPerson: String?
    public var ownerName: String?
    public var agents: [String]
    public var limits: BotGuestLimits?
    public var connected: Bool
    public var createdAt: Int64
    public var joinedAt: Int64?
    public var decidedAt: Int64?
    /// "kai", "owner" or "auto".
    public var decidedBy: String?
    public var lastSeenAt: Int64?
    public var revokedAt: Int64?
    public var revokedWhy: String?
    public var personId: String?
    public var conversations: [BotGuestConversation]
    public var id: String { botId }

    public init(botId: String, name: String, provider: String = "grok", state: BotGuestState, ownerPerson: String? = nil,
                ownerName: String? = nil, agents: [String] = [], limits: BotGuestLimits? = nil, connected: Bool = false,
                createdAt: Int64 = 0, joinedAt: Int64? = nil, decidedAt: Int64? = nil, decidedBy: String? = nil,
                lastSeenAt: Int64? = nil, revokedAt: Int64? = nil, revokedWhy: String? = nil, personId: String? = nil,
                conversations: [BotGuestConversation] = []) {
        self.botId = botId
        self.name = name
        self.provider = provider
        self.state = state
        self.ownerPerson = ownerPerson
        self.ownerName = ownerName
        self.agents = agents
        self.limits = limits
        self.connected = connected
        self.createdAt = createdAt
        self.joinedAt = joinedAt
        self.decidedAt = decidedAt
        self.decidedBy = decidedBy
        self.lastSeenAt = lastSeenAt
        self.revokedAt = revokedAt
        self.revokedWhy = revokedWhy
        self.personId = personId
        self.conversations = conversations
    }

    private enum CodingKeys: String, CodingKey {
        case botId, name, provider, state, ownerPerson, ownerName, agents, limits, connected, createdAt, joinedAt,
             decidedAt, decidedBy, lastSeenAt, revokedAt, revokedWhy, personId, conversations
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        botId = try c.decode(String.self, forKey: .botId)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? botId
        provider = try c.decodeIfPresent(String.self, forKey: .provider) ?? "other"
        state = try c.decodeIfPresent(BotGuestState.self, forKey: .state) ?? .unknown("")
        ownerPerson = try c.decodeIfPresent(String.self, forKey: .ownerPerson)
        ownerName = try c.decodeIfPresent(String.self, forKey: .ownerName)
        agents = try c.decodeIfPresent([String].self, forKey: .agents) ?? []
        limits = try? c.decodeIfPresent(BotGuestLimits.self, forKey: .limits)
        connected = try c.decodeIfPresent(Bool.self, forKey: .connected) ?? false
        createdAt = try c.decodeIfPresent(Int64.self, forKey: .createdAt) ?? 0
        joinedAt = try c.decodeIfPresent(Int64.self, forKey: .joinedAt)
        decidedAt = try c.decodeIfPresent(Int64.self, forKey: .decidedAt)
        decidedBy = try c.decodeIfPresent(String.self, forKey: .decidedBy)
        lastSeenAt = try c.decodeIfPresent(Int64.self, forKey: .lastSeenAt)
        revokedAt = try c.decodeIfPresent(Int64.self, forKey: .revokedAt)
        revokedWhy = try c.decodeIfPresent(String.self, forKey: .revokedWhy)
        personId = try c.decodeIfPresent(String.self, forKey: .personId)
        conversations = try c.decodeIfPresent([BotGuestConversation].self, forKey: .conversations) ?? []
    }

    // MARK: words

    public var providerTitle: String { Self.title(provider: provider) }

    public static func title(provider: String) -> String {
        switch provider {
        case "grok": "Grok"
        case "other": "Other"
        case "": "Bot"
        default: provider.prefix(1).uppercased() + provider.dropFirst()
        }
    }

    public var createdDate: Date { Date(milliseconds: createdAt) }
    public var joined: Date? { joinedAt.map { Date(milliseconds: $0) } }
    public var lastSeen: Date? { lastSeenAt.map { Date(milliseconds: $0) } }
    public var revoked: Date? { revokedAt.map { Date(milliseconds: $0) } }

    /// "house", "house and notes", "house, notes and finance", "house, notes and 2 more".
    public var asks: String {
        switch agents.count {
        case 0: "an agent"
        case 1: agents[0]
        case 2: "\(agents[0]) and \(agents[1])"
        case 3: "\(agents[0]), \(agents[1]) and \(agents[2])"
        default: "\(agents[0]), \(agents[1]) and \(agents.count - 2) more"
        }
    }

    /// Whose bot it is, for a heading: "Sam's bot", or "Your bot" for the computer's owner's own.
    public var ownerLabel: String {
        guard ownerPerson != nil else { return "Your bot" }
        return "\(ownerName?.isEmpty == false ? ownerName! : "Someone")'s bot"
    }

    /// The owner's decision card heading: "Sam's bot · Grok".
    public var requestHeading: String { "\(ownerLabel) · \(providerTitle)" }

    /// The owner's decision card question: "Rex wants to ask house and notes".
    public var requestQuestion: String { "\(name) wants to ask \(asks)" }

    /// How long a bot has waited for an answer ("2 days"); nil under an hour.
    public func waitingAge(at now: Date = .now) -> String? {
        let since = joined ?? createdDate
        let hours = Int(now.timeIntervalSince(since) / 3600)
        guard hours >= 1 else { return nil }
        if hours < 24 { return hours == 1 ? "1 hour" : "\(hours) hours" }
        let days = hours / 24
        return days == 1 ? "1 day" : "\(days) days"
    }
}

/// What `createBotInvite` returns. `share` is the text to paste into the bot, written by the host: shown as it is,
/// never rebuilt, saved or logged.
public struct CreatedBotInvite: Codable, Hashable, Sendable {
    public var botId: String
    public var inviteId: String
    public var expiresAt: Int64
    public var link: String
    public var kithLink: String?
    public var share: String

    public init(botId: String, inviteId: String, expiresAt: Int64, link: String, kithLink: String? = nil, share: String) {
        self.botId = botId
        self.inviteId = inviteId
        self.expiresAt = expiresAt
        self.link = link
        self.kithLink = kithLink
        self.share = share
    }

    public var expiry: Date { Date(milliseconds: expiresAt) }
}
