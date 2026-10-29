import KithKit

/// "Reply" on a message (client-side; the host has no replyTo): the composer shows the quoted line, and the message
/// that goes out starts with it as a markdown blockquote.
enum ReplyQuote {
    /// Longest quote: a pasted wall of text must not become the whole message.
    static let maxLength = 200

    /// The quoted message's first non-empty line; nil when there is nothing to quote (a file with no text).
    static func line(of entry: Entry) -> String? {
        let first = (entry.data.text ?? "").split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }.first { !$0.isEmpty }
        guard let first else { return nil }
        return first.count > maxLength ? String(first.prefix(maxLength)) + "…" : first
    }

    /// What is sent: the quote as a "> " blockquote, a blank line, then what was typed.
    static func compose(quote: String?, text: String) -> String {
        guard let quote, !quote.isEmpty else { return text }
        return "> \(quote)\n\n\(text)"
    }
}
