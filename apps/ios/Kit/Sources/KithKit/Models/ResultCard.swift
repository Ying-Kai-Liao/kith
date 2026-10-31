import Foundation

/// The Result card (DESIGN.md "Reply cards"): what came out of a reply, with its actions. Jev sorted a finished reply
/// and picked which of its own lines fill the slots (host `rich_card::jev`); every string is the reply's own, and the
/// app adds only the fixed words. The entry's text is the card's twin, shown by "Show as text".
/// v2 is the Info card: a title, one sub line and Key: value rows. v1 (a flat facts list) keeps drawing for old entries.
/// Desktop: `result-card.ts`.
public struct ResultCardModel: Hashable, Sendable {
    public struct Link: Hashable, Sendable, Identifiable {
        public var label: String
        public var url: URL
        public var id: String { url.absoluteString }
    }

    public struct Row: Hashable, Sendable {
        public var label: String
        public var value: String
    }

    public var title: String
    /// The verdict or summary line under the title (v2).
    public var sub: String
    /// Key: value rows (v2); empty for a v1 card.
    public var rows: [Row]
    /// Mono facts, drawn joined with " · " (v1); empty for a v2 card.
    public var facts: [String]
    /// Link rows (v2) or buttons (v1) that open a link the reply held.
    public var links: [Link]

    /// Nil (the plain reply) when the card lacks a title or anything to show.
    public init?(card: RichCard) {
        let s = card.snapshot ?? [:]
        let title = Self.text(s["title"])
        let isV2 = card.v >= 2
        var links: [Link] = []
        if case let .array(raw)? = s["links"] { links = raw.compactMap { Self.link($0, bare: isV2) } }
        if isV2 {
            var rows: [Row] = []
            if case let .array(raw)? = s["rows"] {
                // A row with no label draws its value alone; one with no value is dropped.
                rows = raw.compactMap { item in
                    guard case let .object(r) = item else { return nil }
                    let value = Self.text(r["value"])
                    return value.isEmpty ? nil : Row(label: Self.text(r["label"]), value: value)
                }
            }
            guard !title.isEmpty, !rows.isEmpty else { return nil }
            self.sub = Self.text(s["sub"])
            self.rows = Array(rows.prefix(5))
            self.facts = []
        } else {
            var facts: [String] = []
            if case let .array(raw)? = s["facts"] { facts = raw.map { Self.text($0) }.filter { !$0.isEmpty } }
            guard !title.isEmpty, !facts.isEmpty else { return nil }
            self.sub = ""
            self.rows = []
            self.facts = Array(facts.prefix(4))
        }
        self.title = title
        self.links = Array(links.prefix(2))
    }

    private static func text(_ v: RichCard.JSON?) -> String {
        if case let .string(s)? = v { return s.trimmingCharacters(in: .whitespacesAndNewlines) }
        return ""
    }

    /// Only a web link opens from a card. An unlabelled one says Open (v1) or shows its address (v2 link row).
    private static func link(_ raw: RichCard.JSON, bare: Bool) -> Link? {
        guard case let .object(l) = raw else { return nil }
        let link = text(l["url"])
        guard link.range(of: "^https?://", options: [.regularExpression, .caseInsensitive]) != nil,
              let url = URL(string: link) else { return nil }
        let label = text(l["label"])
        let address = link.replacingOccurrences(of: "^https?://", with: "", options: [.regularExpression, .caseInsensitive])
        return Link(label: label.isEmpty ? (bare ? address : "Open") : label, url: url)
    }
}
