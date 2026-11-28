import Foundation
import KithKit

/// The gist of a long agent reply: its first sentence. Pure, so it can be tested without a view.
enum Gist {
    /// A reply is long above this many characters, more than `maxLines` lines, or when it holds a code block.
    static let longCharacters = 280
    static let maxLines = 3

    /// Words whose full stop does not end a sentence.
    private static let abbreviations: Set<String> = [
        "e.g", "i.e", "etc", "vs", "mr", "mrs", "ms", "dr", "prof", "inc", "ltd", "approx", "cf", "st", "no", "fig",
    ]

    /// The first sentence of the first paragraph, plain (no Markdown marks). Nil when the reply does not open with
    /// prose: a code block, a table, an image or a rule has no sentence to stand for it.
    static func sentence(of markdown: String) -> String? {
        var paragraph: [String] = []
        for raw in markdown.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.isEmpty {
                if !paragraph.isEmpty { break }
                continue
            }
            if paragraph.isEmpty, opensNonProse(line) { return nil }
            // A fence, table or list after the first line ends the paragraph.
            if !paragraph.isEmpty, line.hasPrefix("```") || line.hasPrefix("|") || isListItem(line) { break }
            paragraph.append(line)
            // A heading or list item is its own block.
            if paragraph.count == 1, line.hasPrefix("#") || isListItem(line) { break }
        }
        guard !paragraph.isEmpty else { return nil }
        let plain = plainText(paragraph.joined(separator: " "))
        let first = firstSentence(of: plain).trimmingCharacters(in: .whitespacesAndNewlines)
        return first.isEmpty ? nil : first
    }

    /// The gist to show instead of a reply, or nil when the reply stays whole: short, or nothing to fold to.
    static func folded(_ markdown: String) -> String? {
        guard isLong(markdown), let gist = sentence(of: markdown) else { return nil }
        // A reply that is one long sentence has nothing more to show.
        return gist.count < plainText(markdown).trimmingCharacters(in: .whitespacesAndNewlines).count ? gist : nil
    }

    static func isLong(_ markdown: String) -> Bool {
        if markdown.contains("```") { return true }
        if markdown.count > longCharacters { return true }
        return markdown.split(whereSeparator: \.isNewline).filter { !$0.allSatisfy(\.isWhitespace) }.count > maxLines
    }

    private static func opensNonProse(_ line: String) -> Bool {
        if line.hasPrefix("```") || line.hasPrefix("~~~") || line.hasPrefix("|") || line.hasPrefix("![") { return true }
        // A horizontal rule.
        return line.allSatisfy { "-*_ ".contains($0) } && line.filter { !$0.isWhitespace }.count >= 3
    }

    private static func isListItem(_ line: String) -> Bool {
        if line.hasPrefix("- ") || line.hasPrefix("* ") || line.hasPrefix("+ ") { return true }
        let digits = line.prefix { $0.isNumber }
        return !digits.isEmpty && line.dropFirst(digits.count).hasPrefix(". ")
    }

    /// Markdown marks off: heading and list markers, emphasis, code ticks, link targets, images.
    static func plainText(_ markdown: String) -> String {
        var text = markdown
        text = text.replacingOccurrences(of: #"!\[[^\]]*\]\([^)]*\)"#, with: "", options: .regularExpression)
        text = text.replacingOccurrences(of: #"\[([^\]]*)\]\([^)]*\)"#, with: "$1", options: .regularExpression)
        text = text.replacingOccurrences(of: #"(?m)^\s*(#{1,6}\s+|>\s*|[-*+]\s+|\d+\.\s+)"#, with: "", options: .regularExpression)
        for mark in ["**", "__", "~~", "`", "*"] { text = text.replacingOccurrences(of: mark, with: "") }
        return text
    }

    private static func firstSentence(of text: String) -> String {
        let chars = Array(text)
        var i = 0
        while i < chars.count {
            let c = chars[i]
            if "。！？".contains(c) { return String(chars[...i]) }
            if ".!?…".contains(c) {
                // A mark ends the sentence only if a space (or the end) follows it: "3.5" and URLs go on.
                var end = i
                while end + 1 < chars.count, ".!?…\"'”’)".contains(chars[end + 1]) { end += 1 }
                if end + 1 >= chars.count { return String(chars[...end]) }
                if chars[end + 1].isWhitespace, !(c == "." && endsAbbreviation(chars, before: i)) {
                    return String(chars[...end])
                }
                i = end
            }
            i += 1
        }
        return text
    }

    /// The word just before the full stop at `index` is an abbreviation ("e.g", "Dr") or a lone initial.
    private static func endsAbbreviation(_ chars: [Character], before index: Int) -> Bool {
        var start = index
        while start > 0, !chars[start - 1].isWhitespace { start -= 1 }
        let word = String(chars[start..<index]).lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "(\"'“‘"))
        if abbreviations.contains(word) { return true }
        // A lone initial ("J.") or dotted capitals ("U.S.").
        return word.split(separator: ".", omittingEmptySubsequences: false).allSatisfy { $0.count == 1 && $0.first?.isLetter == true }
    }
}

/// Why a reply shows less than it says: it is long (`gist`, in Gist mode), or it belongs to a run someone else
/// prompted in the agent's own chat (`othersRun`, see `OthersRun`; always, Gist mode or not).
enum FoldReason: Equatable {
    case gist(String)
    case othersRun(title: String, count: Int, live: Bool)
}

/// Which replies the chat's Gist mode folds: only an agent's finished, long, plain prose. Asks, warnings, cards, files
/// and the person's own messages never fold, and a reply being written stays whole so the text under the reader's
/// eyes does not change as it grows.
enum ReplyFold {
    static func reason(for entry: Entry, gistMode: Bool) -> FoldReason? {
        guard gistMode, entry.kind == "agent", entry.data.unanswered != true, !entry.data.isLate else { return nil }
        guard !["streaming", "failed", "error"].contains(entry.data.status ?? "") else { return nil }
        guard entry.data.richCard == nil, entry.data.files?.isEmpty != false, entry.data.card == nil else { return nil }
        return Gist.folded(entry.data.text ?? "").map(FoldReason.gist)
    }
}

/// The chat's Gist/Full choice, remembered app-wide.
enum GistSetting {
    static let key = "chat.gistMode"

    /// Pinching in folds, spreading opens; small movements change nothing. Nil: leave the mode as it is.
    static func mode(forPinchScale scale: CGFloat, current: Bool) -> Bool? {
        if scale < 0.8, !current { return true }
        if scale > 1.25, current { return false }
        return nil
    }
}
