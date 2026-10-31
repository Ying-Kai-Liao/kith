import Foundation

/// The Plan card (DESIGN.md "Reply cards"): an agent's plan as one report card that changes in place. The host sends the
/// steps and each one's state; the app adds only the count and the bar. Desktop: `plan-card.ts`.
public struct PlanCardModel: Hashable, Sendable {
    public enum State: String, Hashable, Sendable { case done, now, next, stopped }

    public struct Step: Hashable, Sendable, Identifiable {
        public var id: Int
        public var text: String
        public var state: State
    }

    public var title: String
    /// "3 of 5", drawn in mono.
    public var count: String
    /// 0...1, the bar's fill.
    public var fraction: Double
    public var steps: [Step]
    /// The turn is over: nothing spins.
    public var ended: Bool

    public init(card: RichCard) {
        let s = card.snapshot ?? [:]
        var steps: [Step] = []
        if case let .array(raw)? = s["steps"] {
            for item in raw {
                guard case let .object(o) = item, case let .string(text)? = o["text"], !text.isEmpty else { continue }
                // A state this app doesn't know reads as the quietest, "next".
                var state = State.next
                if case let .string(name)? = o["state"], let known = State(rawValue: name) { state = known }
                steps.append(Step(id: steps.count, text: text, state: state))
            }
        }
        self.steps = steps
        let total = max(Self.number(s["total"]), steps.count)
        let done = min(Self.number(s["done"]), total)
        if case let .string(t)? = s["title"], !t.isEmpty { title = t } else { title = "Plan" }
        count = "\(done) of \(total)"
        fraction = total > 0 ? Double(done) / Double(total) : 0
        if case .bool(true)? = s["ended"] { ended = true } else { ended = false }
    }

    private static func number(_ v: RichCard.JSON?) -> Int {
        if case let .int(n)? = v, n > 0 { return Int(n) }
        return 0
    }
}
