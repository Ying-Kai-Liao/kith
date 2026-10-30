import Foundation

/// Something another app shared to Kith, chosen for one chat and waiting in the App Group until the app sends it.
/// The share extension only queues it: the keys and connections to the computer belong to the app.
public struct PendingShare: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var target: BotReference
    /// The message exactly as it will be sent.
    public var text: String
    public var createdAt: Date

    /// A share nobody opened Kith for this long is dropped rather than arriving out of the blue.
    public static let lifetime: TimeInterval = 7 * 24 * 3600

    public init(id: String = UUID().uuidString, target: BotReference, text: String, createdAt: Date = .now) {
        self.id = id
        self.target = target
        self.text = text
        self.createdAt = createdAt
    }

    public func isExpired(at now: Date) -> Bool { now.timeIntervalSince(createdAt) > Self.lifetime }
}

/// What a share sheet hands over, turned into the message the chat gets.
public enum SharedLink {
    /// The first web link in shared text. Many apps share a link as plain text ("Look at this https://…").
    public static func firstURL(in text: String) -> URL? {
        let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue)
        let range = NSRange(text.startIndex..., in: text)
        for match in detector?.matches(in: text, range: range) ?? [] {
            if let url = match.url, isWeb(url) { return url }
        }
        return nil
    }

    public static func isWeb(_ url: URL) -> Bool { url.scheme == "http" || url.scheme == "https" }

    /// The note, then the page's title when it says something the link doesn't, then the link. Shared text with no
    /// link goes through as typed.
    public static func message(link: URL?, title: String?, text: String?, note: String?) -> String {
        func clean(_ value: String?) -> String? {
            let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed?.isEmpty == false ? trimmed : nil
        }
        var parts = [clean(note)]
        if let link {
            let address = link.absoluteString
            parts.append([clean(title), clean(text)].compactMap { $0 }.first { $0 != address && !$0.contains(address) })
            parts.append(address)
        } else {
            parts.append(clean(text))
        }
        return parts.compactMap { $0 }.joined(separator: "\n\n")
    }
}

public extension SharedStore.Context {
    /// Adds a share for the app to send. The extension calls it; the app calls `clearShares` once sent.
    func queueShare(_ share: PendingShare) {
        pendingShares = pendingShares + [share]
        lastShareTarget = share.target
    }

    func clearShares(ids: Set<String>) {
        guard !ids.isEmpty else { return }
        pendingShares = pendingShares.filter { !ids.contains($0.id) }
    }
}
