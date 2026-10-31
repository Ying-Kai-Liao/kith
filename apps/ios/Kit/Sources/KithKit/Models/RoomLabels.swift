import Foundation

/// A room's purpose labels: filters in the room list, not folders. The limits are the host's (store/model.rs).
public enum RoomLabels {
    public static let maxCount = 5
    public static let maxLength = 24

    /// Labels typed as "launch, q4 plan": lowercase, trimmed, no blanks or repeats, cut to what the host takes.
    public static func parse(_ text: String) -> [String] {
        var seen: [String] = []
        for part in text.split(whereSeparator: { $0 == "," || $0.isNewline }) {
            let words = part.split(whereSeparator: \.isWhitespace).joined(separator: " ").lowercased()
            let label = String(words.prefix(maxLength))
            if !label.isEmpty, !seen.contains(label) { seen.append(label) }
        }
        return Array(seen.prefix(maxCount))
    }

    /// The labels rooms use, most used first (then alphabetical), for the filter row.
    public static func inUse(_ bots: [Bot]) -> [String] {
        var counts: [String: Int] = [:]
        for bot in bots where bot.isGroup {
            for label in bot.labels { counts[label, default: 0] += 1 }
        }
        return counts.sorted { $0.value != $1.value ? $0.value > $1.value : $0.key < $1.key }.map(\.key)
    }

    /// Whether a row shows under the chosen label; none chosen shows everything. Only rooms carry labels.
    public static func matches(_ bot: Bot, label: String?) -> Bool {
        guard let label else { return true }
        return bot.isGroup && bot.labels.contains(label)
    }
}
