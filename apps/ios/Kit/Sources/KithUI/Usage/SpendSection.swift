import KithKit
import SwiftUI

public extension BotStore {
    /// The owner's bot that runs `agent` (not a group): its face and colours on the Usage rows and limit cards.
    func agentBot(_ agent: String) -> Bot? {
        bots.values.first { $0.hqAgent == agent && !$0.isGroup }
    }
}

/// Which agent's sheet is open.
private struct AgentPick: Identifiable, Hashable {
    let agent: String
    var id: String { agent }
}

/// Usage's cost part, under the plan meter: the period's total at API prices split into agents and your own sessions,
/// a row per agent (a thin bar in its colours when it has a limit), your own sessions last. An agent opens its sheet.
/// The computer's owner only; an older host (no `usageReport`) shows nothing.
public struct SpendSection: View {
    let store: BotStore
    @State private var period: SpendPeriod = .month
    @State private var report: UsageReport?
    @State private var error: String?
    @State private var unsupported = false
    @State private var showAll = false
    @State private var open: AgentPick?
    @State private var reload = 0

    public init(store: BotStore) { self.store = store }

    public var body: some View {
        Group {
            if store.isOwner, !unsupported {
                VStack(alignment: .leading, spacing: 14) {
                    SegmentedChoice(selection: $period, options: Spend.periods().map { (id: $0.0, label: $0.1) })
                    if let report {
                        totals(report)
                        agents(report)
                    } else if let error {
                        Text(error).kithText(.secondary).foregroundStyle(Palette.danger)
                    } else {
                        HStack { Spacer(); Spinner(size: 18); Spacer() }.padding(.vertical, 24)
                    }
                }
            }
        }
        .task(id: "\(period.rawValue)/\(reload)") { await load() }
        .kithSheet(item: $open) { pick in
            AgentSpendSheet(store: store, agent: pick.agent) { reload += 1 }
        }
    }

    private func load() async {
        guard store.isOwner else { return }
        do {
            let r = try await store.ready().usageReport(period)
            if !Task.isCancelled { report = r; error = nil }
        } catch where isUnknownMethod(error) {
            unsupported = true
        } catch {
            if !Task.isCancelled, report == nil { self.error = error.localizedDescription }
        }
    }

    private func totals(_ r: UsageReport) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(Spend.money(r.total)).font(.system(size: 34, weight: .bold)).monospacedDigit().foregroundStyle(Palette.ink)
                Text("\(r.label) at API prices").kithText(.secondary).foregroundStyle(Palette.muted)
            }
            .accessibilityElement(children: .combine)
            // Agents in ink, your own sessions in the line colour.
            Capsule().fill(Palette.line).frame(height: 6)
                .overlay(alignment: .leading) {
                    GeometryReader { geo in
                        Capsule().fill(Palette.ink).frame(width: geo.size.width * (r.total > 0 ? r.agents / r.total : 0))
                    }
                }
                .accessibilityHidden(true)
            HStack(spacing: 16) {
                legend("Agents", r.agents, ink: true)
                legend("Your own sessions", r.own, ink: false)
            }
        }
    }

    private func legend(_ title: String, _ amount: Double, ink: Bool) -> some View {
        HStack(spacing: 6) {
            Circle().fill(ink ? Palette.ink : Palette.line).frame(width: 7, height: 7)
            Text(title).foregroundStyle(Palette.muted)
            Text(Spend.money(amount)).fontWeight(.semibold).monospacedDigit().foregroundStyle(Palette.ink)
        }
        .kithText(.secondary)
        .accessibilityElement(children: .combine)
    }

    private func agents(_ r: UsageReport) -> some View {
        let shown = showAll ? r.rows : Array(r.rows.prefix(Spend.shownAgents))
        let more = r.rows.count - shown.count
        return CardSection("Agents") {
            ForEach(shown) { row in
                Button { open = AgentPick(agent: row.agent) } label: { AgentSpendRow(row: row, bot: store.agentBot(row.agent)) }
                    .buttonStyle(.plain)
            }
            HStack(spacing: 12) {
                Image(systemName: "terminal").font(.system(size: 14)).foregroundStyle(Palette.muted)
                    .frame(width: 30, height: 30)
                    .background(Palette.background, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                VStack(alignment: .leading, spacing: 2) {
                    Text("Your own sessions").kithText(.body).foregroundStyle(Palette.secondary)
                    Text("Terminal and flow workers · no agent").kithText(.secondary).foregroundStyle(Palette.muted)
                }
                Spacer(minLength: 8)
                Text(Spend.money(r.own)).kithText(.body).monospacedDigit().foregroundStyle(Palette.secondary)
            }
            .padding(.horizontal, 14).padding(.vertical, 10)
            .accessibilityElement(children: .combine)
        } accessory: {
            if more > 0 || showAll && r.rows.count > Spend.shownAgents {
                Button(showAll ? "Fewer" : "\(more) more") { showAll.toggle() }
                    .buttonStyle(.plain)
                    .kithText(.secondary)
                    .foregroundStyle(Palette.muted)
            }
        }
    }
}

/// face, name, "99 runs · $2.92 a run", "$289 of $300" and its bar, chevron.
private struct AgentSpendRow: View {
    let row: UsageRow
    let bot: Bot?

    var body: some View {
        HStack(spacing: 12) {
            if let bot { BotAvatar(bot: bot, size: 30) } else {
                Image(systemName: "person.crop.circle").foregroundStyle(Palette.muted).frame(width: 30, height: 30)
            }
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline) {
                    Text(bot?.name ?? row.agent).kithText(.body).fontWeight(.semibold).foregroundStyle(Palette.ink).lineLimit(1)
                    Spacer(minLength: 8)
                    Text(row.limit.map { Spend.of(row.month ?? row.cost, $0) } ?? Spend.money(row.cost))
                        .kithText(.body).monospacedDigit().foregroundStyle(Palette.ink)
                }
                Text(Spend.runs(row.runs, perRun: row.perRun)).kithText(.secondary).foregroundStyle(Palette.muted)
                if let limit = row.limit {
                    LimitBar(value: Spend.fullness(row.month ?? row.cost, limit), tint: AgentTint.resolve(bot?.avatarColor))
                        .padding(.top, 3)
                }
            }
            Image(systemName: "chevron.right").font(.footnote).foregroundStyle(Palette.muted)
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityHint("Opens the agent's cost and limit")
    }
}

/// One agent: this month's cost, its limit and forecast, the days, what drives the cost, and the costliest runs.
struct AgentSpendSheet: View {
    let store: BotStore
    let agent: String
    let changed: () -> Void
    @State private var view: AgentUsage?
    @State private var error: String?
    @State private var editing = false

    private var bot: Bot? { store.agentBot(agent) }
    private var tint: AgentTint { AgentTint.resolve(bot?.avatarColor) }

    var body: some View {
        Group {
            if let view {
                if editing {
                    LimitSheet(store: store, view: view, cancel: { editing = false }) { saved in
                        self.view = saved
                        editing = false
                        changed()
                    }
                } else {
                    content(view)
                }
            } else if let error {
                Text(error).kithText(.secondary).foregroundStyle(Palette.danger).padding(20)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            } else {
                Spinner(size: 20).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) {
            if !editing {
                ModalHeader {
                    HStack(spacing: 8) {
                        if let bot { BotAvatar(bot: bot, size: 24) }
                        Text(bot?.name ?? agent)
                    }
                }
            }
        }
        .task { await load() }
    }

    private func load() async {
        do {
            view = try await store.ready().agentUsage(agent, period: .month)
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func content(_ v: AgentUsage) -> some View {
        CardForm {
            VStack(alignment: .leading, spacing: 10) {
                Text(Spend.money(v.month, cents: true)).font(.system(size: 34, weight: .bold)).monospacedDigit().foregroundStyle(Palette.ink)
                Text("\(v.monthLabel) · \(Spend.runs(v.runs, perRun: v.perRun)) · at API prices").kithText(.secondary).foregroundStyle(Palette.muted)
                if let limit = v.limit {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Text("Limit").foregroundStyle(Palette.muted)
                            Spacer()
                            Text(Spend.of(v.month, limit)).monospacedDigit().foregroundStyle(Palette.ink)
                        }
                        .kithText(.secondary)
                        LimitBar(value: Spend.fullness(v.month, limit), tint: tint, height: 5)
                    }
                    .padding(.top, 4)
                }
                if let line = Spend.agentForecast(v) {
                    Text(line).kithText(.secondary).foregroundStyle(Palette.secondary).fixedSize(horizontal: false, vertical: true)
                }
                DayBars(days: v.days)
            }
            CardSection("Limit") {
                Button { editing = true } label: { linkRow("Monthly limit", v.limit.map { Spend.money($0) } ?? "No limit") }
                    .buttonStyle(.plain)
                Button { editing = true } label: { linkRow("At the limit", v.at.short) }
                    .buttonStyle(.plain)
                VStack(alignment: .leading, spacing: 2) {
                    valueRow("Model and effort", Spend.modelLine(v.models))
                    Text("Most of the cost is chosen here").kithText(.secondary).foregroundStyle(Palette.muted)
                }
                .padding(.horizontal, 14).padding(.vertical, 10)
            }
            if !v.costliest.isEmpty {
                CardSection("Costliest runs") {
                    ForEach(v.costliest) { run in
                        HStack(spacing: 12) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(run.title).kithText(.body).foregroundStyle(Palette.ink).lineLimit(1)
                                Text([Spend.shortDay(run.started), run.via].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · "))
                                    .kithText(.secondary).foregroundStyle(Palette.muted)
                            }
                            Spacer(minLength: 8)
                            Text(Spend.money(run.cost, cents: true)).kithText(.body).monospacedDigit().foregroundStyle(Palette.ink)
                        }
                        .padding(.horizontal, 14).padding(.vertical, 10)
                        .accessibilityElement(children: .combine)
                    }
                }
            }
        }
    }

    private func linkRow(_ title: String, _ value: String) -> some View {
        HStack(spacing: 8) {
            valueRow(title, value)
            Image(systemName: "chevron.right").font(.footnote).foregroundStyle(Palette.muted)
        }
        .padding(.horizontal, 14).padding(.vertical, 12)
        .contentShape(Rectangle())
    }

    private func valueRow(_ title: String, _ value: String) -> some View {
        HStack {
            Text(title).kithText(.body).foregroundStyle(Palette.ink)
            Spacer(minLength: 8)
            Text(value).kithText(.body).foregroundStyle(Palette.muted)
        }
    }
}

/// This month's days as ink bars, the first and today labelled.
private struct DayBars: View {
    let days: [AgentUsage.Day]

    var body: some View {
        if let first = days.first {
            let top = max(0.01, days.map(\.cost).max() ?? 0)
            VStack(spacing: 4) {
                HStack(alignment: .bottom, spacing: 2) {
                    ForEach(days, id: \.day) { d in
                        RoundedRectangle(cornerRadius: 1).fill(Palette.ink)
                            .frame(height: max(d.cost > 0 ? 3 : 1, 64 * d.cost / top))
                            .frame(maxWidth: .infinity)
                    }
                }
                .frame(height: 64, alignment: .bottom)
                .overlay(alignment: .bottom) { Rectangle().fill(Palette.line).frame(height: 1) }
                HStack {
                    Text(Spend.shortDay(first.day))
                    Spacer()
                    Text("Today")
                }
                .kithText(.label).foregroundStyle(Palette.muted)
            }
            .padding(.top, 6)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Cost per day this month")
        }
    }
}

/// The limit sheet: presets or a custom amount, the forecast in words, and what happens at the limit.
struct LimitSheet: View {
    let store: BotStore
    let view: AgentUsage
    let cancel: () -> Void
    let saved: (AgentUsage) -> Void
    @State private var limit: Double?
    @State private var custom: String
    @State private var customOn: Bool
    @State private var at: AtLimit
    @State private var busy = false
    @State private var error: String?

    init(store: BotStore, view: AgentUsage, cancel: @escaping () -> Void, saved: @escaping (AgentUsage) -> Void) {
        self.store = store
        self.view = view
        self.cancel = cancel
        self.saved = saved
        let known = view.limit == nil || Spend.limitPresets.contains(view.limit!)
        _limit = State(initialValue: view.limit)
        _custom = State(initialValue: known ? "" : String(format: "%g", view.limit!))
        _customOn = State(initialValue: !known)
        _at = State(initialValue: view.at)
    }

    private var typed: Double? { Double(custom.trimmingCharacters(in: .whitespaces)).flatMap { $0 > 0 ? $0 : nil } }
    private var chosen: Double? { customOn ? typed : limit }
    private var bot: Bot? { store.agentBot(view.agent) }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                IconButton("Cancel", systemImage: "xmark") { cancel() }
                Spacer()
                Button {
                    Task { await done() }
                } label: {
                    ZStack {
                        Text("Done").opacity(busy ? 0 : 1)
                        if busy { Spinner(size: 14) }
                    }
                    .frame(minWidth: 44)
                }
                .buttonStyle(.ink)
                .disabled(busy || customOn && typed == nil)
            }
            .padding(.horizontal, 16).frame(height: 56)
            CardForm {
                HStack(spacing: 12) {
                    if let bot { BotAvatar(bot: bot, size: 36) }
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Monthly limit").kithText(.agentName).foregroundStyle(Palette.ink)
                        Text("\(bot?.name ?? view.agent) · starts again \(Spend.nextMonthStart())").kithText(.secondary).foregroundStyle(Palette.muted)
                    }
                }
                VStack(alignment: .leading, spacing: 10) {
                    LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 8), count: 3), spacing: 8) {
                        preset("No limit", on: !customOn && limit == nil) { customOn = false; limit = nil }
                        ForEach(Spend.limitPresets, id: \.self) { p in
                            preset(Spend.money(p), on: !customOn && limit == p) { customOn = false; limit = p }
                        }
                        preset("Custom", on: customOn) { customOn = true }
                    }
                    if customOn {
                        HStack(spacing: 6) {
                            Text("$").foregroundStyle(Palette.muted)
                            TextField("Amount", text: $custom).keyboardType(.decimalPad)
                            Text("a month").foregroundStyle(Palette.muted)
                        }
                        .padding(.horizontal, 12).frame(height: 40)
                        .background(Palette.panel, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    }
                    Text(Spend.forecast(view.forecast, spent: view.month, chosen: chosen))
                        .kithText(.secondary).foregroundStyle(Palette.secondary).fixedSize(horizontal: false, vertical: true)
                }
                CardSection("At the limit", footer: "Your own runs never just stop at the limit: Ask me gives you a card, and Hold is only there if you pick it.") {
                    ForEach(AtLimit.allCases) { choice in
                        Button { at = choice } label: {
                            HStack(alignment: .top, spacing: 10) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(choice.title).kithText(.body).foregroundStyle(Palette.ink)
                                    Text(choice == .hold ? "They wait until \(Spend.nextMonthStart()) or until you raise it" : choice.detail)
                                        .kithText(.secondary).foregroundStyle(Palette.muted)
                                }
                                Spacer(minLength: 8)
                                if at == choice { Image(systemName: "checkmark").fontWeight(.semibold).foregroundStyle(Palette.ink) }
                            }
                            .padding(.horizontal, 14).padding(.vertical, 10)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(at == choice ? .isSelected : [])
                    }
                }
                if let error { Text(error).kithText(.secondary).foregroundStyle(Palette.danger) }
            }
        }
        .background(Palette.background)
    }

    private func preset(_ title: String, on: Bool, pick: @escaping () -> Void) -> some View {
        Button(action: pick) {
            Text(title).kithText(.body).fontWeight(on ? .semibold : .regular).monospacedDigit()
                .foregroundStyle(on ? Palette.background : Palette.ink)
                .frame(maxWidth: .infinity, minHeight: 40)
                .background(on ? Palette.ink : Palette.panel, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
    }

    private func done() async {
        busy = true
        defer { busy = false }
        do {
            saved(try await store.ready().setAgentLimit(view.agent, limit: chosen, at: at))
        } catch {
            self.error = error.localizedDescription
        }
    }
}
