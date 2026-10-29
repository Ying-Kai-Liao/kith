import Foundation

/// The one pill line an answered or finished card shrinks to ("Allowed npm test"); same words as the Mac's fold-card.ts.
public struct CardFold: Equatable, Sendable {
    /// A green check; false is a red x (denied, not now, expired).
    public var ok: Bool
    public var text: String

    /// Nil while the card is pending, or while an old run still holds the session an approval resumes in (that card
    /// keeps its held row, so it stays whole).
    public static func of(_ entry: Entry) -> CardFold? {
        let d = entry.data
        if d.isSecretCard, let s = d.secret {
            let access = d.card == "account"
            switch s.state {
            case "pending": return nil
            case "saved":
                return CardFold(ok: true, text: access ? "Allowed \(s.service)\(s.account.map { " · \($0)" } ?? "")"
                                                        : "Saved the key for \(s.profile ?? s.service)")
            case "denied": return CardFold(ok: false, text: "Not now · \(s.service) \(access ? "access" : "key")")
            default: return CardFold(ok: false, text: "Expired · \(s.service) \(access ? "access" : "key")")
            }
        }
        guard let status = d.status, status != "pending" else { return nil }
        if status == "answered", d.held != nil { return nil }
        if d.limit != nil {
            return CardFold(ok: !(status == "cancelled" || d.selected == "stop"), text: Spend.outcome(entry))
        }
        let what = d.title ?? ""
        if status == "answered", let kind = d.options?.first(where: { $0.optionId == d.selected })?.kind {
            let lead: String?
            switch kind {
            case "allow_once": lead = "Allowed"
            case "allow_always": lead = "Always allowed"
            case "reject_once": lead = "Denied"
            case "reject_always": lead = "Never allowed"
            default: lead = nil
            }
            if let lead { return CardFold(ok: kind.hasPrefix("allow"), text: "\(lead) \(what)".trimmingCharacters(in: .whitespaces)) }
        }
        let outcome = status == "answered" ? (d.outcome?.isEmpty == false ? d.outcome! : "Answered")
            : status == "cancelled" ? "Cancelled" : "Expired"
        return CardFold(ok: status == "answered", text: what.isEmpty ? outcome : "\(outcome) · \(what)")
    }
}

/// The user's reactions grouped for display: each emoji once, with how many times it appears (old data repeats them).
public enum ReactionGroups {
    public static func of(_ reactions: [String]) -> [(emoji: String, count: Int)] {
        var order: [String] = []
        var counts: [String: Int] = [:]
        for r in reactions {
            if counts[r] == nil { order.append(r) }
            counts[r, default: 0] += 1
        }
        return order.map { ($0, counts[$0] ?? 0) }
    }
}

/// The line under a tapped agent reply: the time, and the context ring when the entry carries a percentage.
public enum ContextLine {
    public static func label(percent: Int?) -> String? { percent.map { "\($0)% context" } }
    /// The ring's arc as a 0...1 fraction.
    public static func fraction(_ percent: Int) -> Double { Double(min(max(percent, 0), 100)) / 100 }
}
