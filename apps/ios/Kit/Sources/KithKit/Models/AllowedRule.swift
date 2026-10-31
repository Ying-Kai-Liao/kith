import Foundation

/// One thing "Always allow" remembered for an agent in auto mode (`allowedRules`, serve/host/src/cards/autoallow.rs).
public struct AllowedRule: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var rule: String
    /// "rule" (a permission rule) or "prose" (a sentence for the classifier).
    public var kind: String
    public var reason: String
    /// ISO 8601, or nil when the file doesn't say.
    public var added: String?

    public init(id: String, rule: String, kind: String = "rule", reason: String = "", added: String? = nil) {
        self.id = id
        self.rule = rule
        self.kind = kind
        self.reason = reason
        self.added = added
    }

    /// A permission rule as it is; a sentence cut to one short line.
    public func text(max: Int = 90) -> String {
        guard kind == "prose", rule.count > max else { return rule }
        return rule.prefix(max - 1).trimmingCharacters(in: .whitespaces) + "…"
    }

    public var addedDate: Date? {
        guard let added else { return nil }
        return ISO8601DateFormatter().date(from: added)
    }

    /// "Blocked: [Data Exfiltration] · 2 h ago".
    public func detail(now: Date = .now, ago: (Date, Date) -> String) -> String? {
        let parts = [reason.isEmpty ? nil : "Blocked: \(reason)", addedDate.map { ago($0, now) }].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

/// `allowedRules`' answer: the rules, and `error` when the file could not be read whole (the readable ones are still listed).
public struct AllowedList: Decodable, Sendable, Equatable {
    public var allowed: [AllowedRule]
    public var error: String?

    public init(allowed: [AllowedRule] = [], error: String? = nil) {
        self.allowed = allowed
        self.error = error
    }
}
