import Foundation

/// The News card (DESIGN.md "News"): a scheduled run's brief as rows, each a title, one line and "source · domain" that
/// opens its link. The host found the rows in the text the run sent; the app adds only the fixed words.
/// Desktop: `news-card.ts`.
public struct NewsCardModel: Hashable, Sendable {
    public struct Row: Hashable, Sendable, Identifiable {
        public var title: String
        public var line: String
        public var source: String?
        public var domain: String
        public var url: URL
        public var id: String { url.absoluteString }
    }

    /// Three rows show; the rest sit behind "Show N more".
    public static let rowsShown = 3

    /// "Morning brief · Sat 25 Oct".
    public var title: String
    /// "every day 7:00", or nil for a job the host couldn't name the schedule of.
    public var every: String?
    public var lead: String
    public var tail: String
    public var rows: [Row]
    /// The rows shown before "Show N more".
    public var shown: [Row]
    public var more: Int
    /// After its day: "Morning brief · Fri · 6 items"; tap opens it.
    public var fold: String?

    public init(card: RichCard, postedAt: Date, now: Date = .now, expanded: Bool = false, opened: Bool = false,
                calendar: Calendar = .current) {
        let s = card.snapshot ?? [:]
        if case let .array(raw)? = s["rows"] { rows = raw.compactMap(Self.row) } else { rows = [] }
        let name = Self.text(s["name"]).isEmpty ? "News" : Self.text(s["name"])
        let weekday = CardWords.shortDays[calendar.component(.weekday, from: postedAt) - 1]
        let day = "\(calendar.component(.day, from: postedAt)) \(CardWords.months[calendar.component(.month, from: postedAt) - 1])"
        title = "\(name) · \(weekday) \(day)"
        every = Self.text(s["every"]).isEmpty ? nil : CardWords.quietEvery(Self.text(s["every"]))
        lead = Self.text(s["lead"])
        tail = Self.text(s["tail"])
        let all = expanded || opened || rows.count <= Self.rowsShown
        shown = all ? rows : Array(rows.prefix(Self.rowsShown))
        more = rows.count - shown.count
        let past = CardWords.days(postedAt, from: now, calendar: calendar) < 0
        fold = past && !opened ? "\(name) · \(weekday) · \(rows.count) \(rows.count == 1 ? "item" : "items")" : nil
    }

    private static func text(_ v: RichCard.JSON?) -> String {
        if case let .string(s)? = v { return s }
        return ""
    }

    /// A row the app may open: an http(s) link with a title.
    private static func row(_ raw: RichCard.JSON) -> Row? {
        guard case let .object(r) = raw else { return nil }
        let link = text(r["url"])
        guard !text(r["title"]).isEmpty, link.range(of: "^https?://", options: [.regularExpression, .caseInsensitive]) != nil,
              let url = URL(string: link) else { return nil }
        let source = text(r["source"])
        return Row(title: text(r["title"]), line: text(r["line"]), source: source.isEmpty ? nil : source, domain: text(r["domain"]), url: url)
    }
}
