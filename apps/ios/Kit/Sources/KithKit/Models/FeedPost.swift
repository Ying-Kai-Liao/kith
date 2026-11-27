import Foundation

/// What a post's open-ended `attachment` and `items` hold, read for the cards. An attachment of a type this app
/// doesn't know is `.unknown`: its post shows as a plain card.
public enum FeedAttachment: Equatable, Sendable {
    case file(SharedFile)
    case page(file: SharedFile?, shot: SharedFile?, version: String?)
    case video(SharedFile)
    case number(value: Double, unit: String?, delta: Double?)
    case link(URL)
    case unknown

    public init?(_ value: HostContract.JSONValue?) {
        guard let value, case let .object(o) = value else { return nil }
        func file(_ key: String) -> SharedFile? {
            guard let v = o[key], case .object = v, let data = try? JSONEncoder().encode(v) else { return nil }
            return try? JSONDecoder().decode(SharedFile.self, from: data)
        }
        func number(_ key: String) -> Double? {
            switch o[key] {
            case let .int(i): Double(i)
            case let .double(d): d
            default: nil
            }
        }
        func string(_ key: String) -> String? {
            if case let .string(s)? = o[key] { s } else { nil }
        }
        switch string("type") {
        case "file": self = file("file").map(Self.file) ?? .unknown
        case "video": self = file("file").map(Self.video) ?? .unknown
        case "page": self = .page(file: file("file"), shot: file("shot"), version: string("version"))
        case "number": self = number("value").map { .number(value: $0, unit: string("unit"), delta: number("delta")) } ?? .unknown
        case "link":
            // https only, like the host's own rule.
            self = string("url").flatMap(URL.init(string:)).flatMap { $0.scheme == "https" ? $0 : nil }.map(Self.link) ?? .unknown
        default: self = .unknown
        }
    }
}

/// One merged PR in a `merges` post.
public struct FeedMerge: Equatable, Sendable, Identifiable {
    public let id: String
    public let title: String
}

public extension HostContract.Post {
    var feedAttachment: FeedAttachment? { FeedAttachment(attachment) }

    /// A merges post's PRs; an item without a title shows its id.
    var merges: [FeedMerge] {
        items.compactMap { item in
            guard case let .object(o) = item, case let .string(id)? = o["id"] else { return nil }
            if case let .string(title)? = o["title"], !title.isEmpty { return FeedMerge(id: id, title: title) }
            return FeedMerge(id: id, title: id)
        }
    }
}

public extension HostContract.Story {
    /// A ring lasts three hours from the agent's last word, whether or not the host has refetched.
    static let life: Int64 = 3 * 3600 * 1000

    func isLive(nowMs: Int64) -> Bool { lastAt + Self.life > nowMs }
}
