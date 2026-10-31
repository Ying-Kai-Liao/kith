import Foundation

/// A card drawn over an entry, `data.richCard`: the host makes it from a tool result or a run's outbox item, and the
/// entry's `text` stays its twin (Telegram, Discord, Slack, notifications, and any app that can't draw it). The
/// permission entries' `card` is the other kind, the Access and Key cards.
///
/// Decoding never fails the entry: anything that isn't a card decodes with an empty type, which no app draws.
public struct RichCard: Codable, Hashable, Sendable {
    public typealias JSON = HostContract.JSONValue

    /// The card types this app draws, each with the newest version it knows.
    public static let known: [String: Int] = ["schedule": 1, "news": 1, "result": 1, "plan": 1]

    /// schedule · routine · news · result; open string.
    public var type: String
    /// The version of that type's shape; an app draws only the versions it knows.
    public var v: Int
    /// The live record (a routine's id, an agent and its job's id): the app shows its current state.
    public var ref: [String: JSON]?
    /// The record as it was when the card was made, for when it is gone or the host can't say.
    public var snapshot: [String: JSON]?

    public init(type: String, v: Int, ref: [String: JSON]? = nil, snapshot: [String: JSON]? = nil) {
        self.type = type
        self.v = v
        self.ref = ref
        self.snapshot = snapshot
    }

    public init(from decoder: Decoder) throws {
        let value = (try? decoder.singleValueContainer().decode(JSON.self)) ?? .null
        guard case let .object(o) = value, case let .string(type)? = o["type"], case let .int(v)? = o["v"],
              let v = Int(exactly: v) else {
            self.init(type: "", v: 0)
            return
        }
        func object(_ key: String) -> [String: JSON]?? {
            switch o[key] {
            case nil, .null?: .some(nil)
            case let .object(value)?: .some(value)
            default: nil
            }
        }
        guard let ref = object("ref"), let snapshot = object("snapshot") else {
            self.init(type: "", v: 0)
            return
        }
        self.init(type: type, v: v, ref: ref, snapshot: snapshot)
    }

    /// Whether this app draws it: a type it knows, at a version it knows. Otherwise the entry is the plain row it
    /// always was.
    public func isDrawable(known: [String: Int] = Self.known) -> Bool {
        guard let newest = known[type] else { return false }
        return v >= 1 && v <= newest
    }
}
