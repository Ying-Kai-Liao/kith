import KithKit
import SwiftUI

/// What each person spent on one agent this month and their limit on it (Access > People): loaded once per sheet,
/// written back by the owner. Nothing shows for a non-owner or an older host.
@MainActor @Observable
public final class PeopleSpend {
    public private(set) var people: AgentPeople?
    public private(set) var error: String?
    public private(set) var busy: String?
    let store: BotStore
    let agent: String

    public init(store: BotStore, agent: String) {
        self.store = store
        self.agent = agent
    }

    public func load() async {
        guard store.isOwner, !agent.isEmpty else { return }
        do {
            people = try await store.ready().agentPeople(agent)
        } catch where isUnknownMethod(error) {
            return
        } catch {
            self.error = error.localizedDescription
        }
    }

    public func setBudget(_ person: String, _ budget: Double?) async {
        busy = person
        error = nil
        defer { busy = nil }
        do {
            people = try await store.ready().setPersonBudget(agent, person: person, budget: budget)
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// "$20.10 of $20", or what they spent when there is no limit; nil before it loads.
    public func line(for person: String) -> String? { people?.line(for: person) }
}

/// The owner's own row: their spend here; their limit is the agent's, set in Usage.
public struct OwnerSpendRow: View {
    let spend: PeopleSpend

    public init(spend: PeopleSpend) { self.spend = spend }

    public var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "person.crop.circle").font(.system(size: 22)).foregroundStyle(Palette.secondary).frame(width: 30, height: 30)
            VStack(alignment: .leading, spacing: 2) {
                Text("You").font(.body.weight(.semibold))
                Text([spend.line(for: "owner"), "Owner", "your limit is in Usage"].compactMap { $0 }.joined(separator: " · "))
                    .font(.subheadline).foregroundStyle(Palette.secondary)
            }
            Spacer()
        }
        .accessibilityElement(children: .combine)
    }
}

/// The thin bar under a person's line, in the agent's colours; none without a limit.
public struct PersonSpendBar: View {
    let spend: PeopleSpend
    let person: String

    public init(spend: PeopleSpend, person: String) {
        self.spend = spend
        self.person = person
    }

    public var body: some View {
        if let people = spend.people, case let s = people.spend(of: person), let budget = s.budget {
            LimitBar(value: Spend.fullness(s.spent, budget), tint: AgentTint.resolve(spend.store.agentBot(spend.agent)?.avatarColor))
                .padding(.top, 4)
        }
    }
}

/// LIMIT FOR each person who holds the agent: what their runs may cost a month before they stop and you get a card.
public struct PeopleLimits: View {
    let spend: PeopleSpend
    let holders: [(id: String, name: String)]

    public init(spend: PeopleSpend, holders: [(id: String, name: String)]) {
        self.spend = spend
        self.holders = holders
    }

    public var body: some View {
        if let people = spend.people, !holders.isEmpty {
            CardSection("Limits each month",
                        footer: "A person's runs stop at their limit and you get a card. It counts runs they start and helpers' calls for them, at API prices. New people start at $20 a month.") {
                ForEach(holders, id: \.id) { h in
                    BudgetRow(name: h.name, line: people.line(for: h.id), budget: people.spend(of: h.id).budget, busy: spend.busy != nil) { b in
                        Task { await spend.setBudget(h.id, b) }
                    }
                }
            }
        }
    }
}

/// "Limit for your bots": what the owner's bots may spend together on every agent before they stop and you get a card.
/// Off by default; nothing shows for a non-owner or an older host.
public struct BotsCapRow: View {
    let store: BotStore
    @State private var cap: BotsCap?
    @State private var error: String?
    @State private var busy = false

    public init(store: BotStore) { self.store = store }

    public var body: some View {
        Group {
            if let cap {
                BudgetRow(name: "your bots", line: error ?? "\(cap.line) · all agents", budget: cap.cap, busy: busy) { b in
                    Task { await set(b) }
                }
            }
        }
        .task {
            guard store.isOwner else { return }
            do {
                cap = try await store.ready().botsCap()
            } catch where isUnknownMethod(error) {
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func set(_ b: Double?) async {
        busy = true
        error = nil
        defer { busy = false }
        do {
            cap = try await store.ready().setBotsCap(b)
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// "Limit for Mia · Each month $20": the stock pop-up with a custom amount.
struct BudgetRow: View {
    let name: String
    let line: String
    let budget: Double?
    let busy: Bool
    let set: (Double?) -> Void
    @State private var choice = ""
    @State private var custom = ""

    private static let none = "none"
    private static let customId = "custom"
    private var preset: String {
        guard let budget else { return Self.none }
        return Spend.budgetPresets.contains(budget) ? String(Int(budget)) : Self.customId
    }
    private var options: [(id: String, label: String)] {
        Spend.budgetPresets.map { (id: String(Int($0)), label: Spend.money($0)) } + [(id: Self.none, label: "No limit"), (id: Self.customId, label: "Custom")]
    }
    private var typed: Double? { Double(custom.trimmingCharacters(in: .whitespaces)).flatMap { $0 > 0 ? $0 : nil } }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Limit for \(name)").font(.body.weight(.semibold)).lineLimit(1)
                    Text(line).font(.subheadline).monospacedDigit().foregroundStyle(Palette.secondary)
                }
                Spacer(minLength: 8)
                Text("Each month").font(.subheadline).foregroundStyle(Palette.secondary)
                ChoicePicker(selection: Binding(get: { choice.isEmpty ? preset : choice }, set: { pick($0) }), options: options)
                    .disabled(busy)
                    .accessibilityLabel("Limit for \(name), each month")
            }
            if (choice.isEmpty ? preset : choice) == Self.customId {
                HStack(spacing: 6) {
                    Text("$").foregroundStyle(Palette.muted)
                    TextField("25", text: $custom).keyboardType(.decimalPad)
                        .accessibilityLabel("Custom limit for \(name)")
                    Button { if let typed { set(typed) } } label: {
                        Text("Set").font(.system(size: 15, weight: .semibold)).padding(.horizontal, 12).frame(height: 32)
                    }
                        .buttonStyle(.ink)
                        .disabled(busy || typed == nil || typed == budget)
                }
                .padding(.leading, 12).padding(.trailing, 4).frame(height: 40)
                .background(Palette.background, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
        }
        .onAppear { if preset == Self.customId, let budget { custom = String(format: "%g", budget) } }
        .onChange(of: budget) { choice = "" }
    }

    private func pick(_ id: String) {
        choice = id
        switch id {
        case Self.none: set(nil)
        case Self.customId: break
        default: if let v = Double(id) { set(v) }
        }
    }
}
