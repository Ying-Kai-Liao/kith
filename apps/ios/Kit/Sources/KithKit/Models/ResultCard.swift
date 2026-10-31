import Foundation

/// The Result card (DESIGN.md "Reply cards"): what came out of a reply, with its actions. Jev sorted a finished reply
/// and picked which of its own lines fill the slots (host `rich_card::jev`); every string is the reply's own, and the
/// app adds only the fixed words. The entry's text is the card's twin, shown by "Show as text".
/// Desktop: `result-card.ts`.
public struct ResultCardModel: Hashable, Sendable {
    public struct Link: Hashable, Sendable, Identifiable {
        public var label: String
        public var url: URL
        public var id: String { url.absoluteString }
    }

    public var title: String
    /// Mono facts, drawn joined with " · ".
    public var facts: [String]
    /// Buttons that open a link the reply held.
    public var links: [Link]

    /// Nil (the plain reply) when the card lacks a title or a fact.
    public init?(card: RichCard) {
        let s = card.snapshot ?? [:]
        let title = Self.text(s["title"])
        var facts: [String] = []
        if case let .array(raw)? = s["facts"] { facts = raw.map { Self.text($0) }.filter { !$0.isEmpty } }
        guard !title.isEmpty, !facts.isEmpty else { return nil }
        var links: [Link] = []
        if case let .array(raw)? = s["links"] { links = raw.compactMap(Self.link) }
        self.title = title
        self.facts = Array(facts.prefix(4))
        self.links = Array(links.prefix(2))
    }

    private static func text(_ v: RichCard.JSON?) -> String {
        if case let .string(s)? = v { return s.trimmingCharacters(in: .whitespacesAndNewlines) }
        return ""
    }

    /// Only a web link opens from a card.
    private static func link(_ raw: RichCard.JSON) -> Link? {
        guard case let .object(l) = raw else { return nil }
        let link = text(l["url"])
        guard link.range(of: "^https?://", options: [.regularExpression, .caseInsensitive]) != nil,
              let url = URL(string: link) else { return nil }
        let label = text(l["label"])
        return Link(label: label.isEmpty ? "Open" : label, url: url)
    }
}
