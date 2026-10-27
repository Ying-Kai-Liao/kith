import KithKit
import SwiftUI

/// "2 min ago", "3 h ago", "4 d ago", or the date.
func kithAgo(_ date: Date, now: Date = .now) -> String {
    let s = max(0, now.timeIntervalSince(date))
    switch s {
    case ..<60: return "just now"
    case ..<3600: return "\(Int(s / 60)) min ago"
    case ..<86_400: return "\(Int(s / 3600)) h ago"
    case ..<(86_400 * 30): return "\(Int(s / 86_400)) d ago"
    default: return date.formatted(.dateTime.month(.abbreviated).day().year())
    }
}

func kithBytes(_ n: Int) -> String {
    ByteCountFormatter.string(fromByteCount: Int64(n), countStyle: .file)
}

/// The package chip under a kith bot's name, mono: `kith · coder 1.4`. Opens a menu of where the package comes from
/// (changes, unpushed, pinned, version).
struct KithIdentityLine: View {
    let agent: String
    let store: KithInfoStore

    var body: some View {
        if store.info?.errors["package"] != nil {
            Text("Couldn't read the package").font(.system(size: 11.5)).foregroundStyle(Palette.muted)
        } else if let package = store.info?.package, let chip = package.chip {
            DropdownMenu {
                var items: [MenuItem] = []
                if let remote = package.remote { items.append(MenuItem(remote, icon: "link") {}) }
                if package.dirty { items.append(MenuItem("Uncommitted changes", icon: "pencil") {}) }
                if package.unpushed { items.append(MenuItem("Not pushed", icon: "arrow.up") {}) }
                if let pinned = package.pinned { items.append(MenuItem("Pinned to \(pinned)", icon: "pin") {}) }
                items.append(MenuItem("Package version \(package.version ?? chip)", icon: "shippingbox") {})
                return items
            } label: {
                HStack(spacing: 4) {
                    Text("kith · \(agent) \(chip)")
                    Image(systemName: "chevron.up.chevron.down").font(.system(size: 8, weight: .semibold))
                }
                .font(KithText.code.font)
                .foregroundStyle(Palette.muted)
                .padding(.horizontal, 9)
                .padding(.vertical, 3)
                .background(Palette.panel, in: Capsule())
            }
            .accessibilityLabel("Package \(agent) \(chip)")
        } else {
            Text("kith · \(agent)").font(KithText.code.font).foregroundStyle(Palette.muted)
        }
    }
}

/// The Details page for a kith bot, top to bottom (serve/docs/bot-info.md, app-style/agent-sheet.png): the host's
/// About rows, Cost, Schedules & routines (one list), Projects, Harness, then Skills, Tools and Plugins.
struct KithDetailsSections<About: View, Routines: View>: View {
    let store: KithInfoStore
    let tint: AgentTint
    let about: About
    /// The routines list, given kith's schedules to show as read-only rows and whether it couldn't read them.
    let routines: ([KithSchedule], Bool) -> Routines
    @State private var period = CostPeriod.month

    init(store: KithInfoStore, tint: AgentTint, @ViewBuilder about: () -> About,
         @ViewBuilder routines: @escaping ([KithSchedule], Bool) -> Routines) {
        self.store = store
        self.tint = tint
        self.about = about()
        self.routines = routines
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            switch store.phase {
            case .unavailable:
                about
                routines([], false)
            case .loading:
                about
                CardSection { SheetNote(text: "Loading kith details…") }
                routines([], false)
            case .loaded(let response):
                if let info = response.info {
                    if let failed = store.refreshError {
                        SheetNote(text: "Refresh failed: \(failed)", danger: true) { Task { await store.load(refresh: true) } }
                    }
                    sections(info)
                } else {
                    if let error = response.error {
                        SheetNote(text: "Kith details unavailable: \(error)", danger: true) { Task { await store.load(refresh: true) } }
                    }
                    about
                    routines([], false)
                }
            }
        }
    }

    @ViewBuilder private func sections(_ info: KithInfo) -> some View {
        about
        cost(info)
        routines(info.schedules, info.errors["schedules"] != nil)
        projects(info)
        harness(info)
        access
        capabilities(info)
    }

    // MARK: cost

    enum CostPeriod: String, CaseIterable { case today, week, month, all
        var label: String {
            switch self {
            case .today: "Today"
            case .week: "7 d"
            case .month: "30 d"
            case .all: "All"
            }
        }
    }

    @ViewBuilder private func cost(_ info: KithInfo) -> some View {
        CardSection("Cost") {
            if info.errors["cost"] != nil {
                SheetNote(text: "Couldn't read the cost")
            } else if let cost = info.cost {
                VStack(alignment: .leading, spacing: 12) {
                    SegmentedChoice(selection: $period, options: CostPeriod.allCases.map { ($0, $0.label) })
                    HStack(alignment: .firstTextBaseline) {
                        Text(KithCost.format(amount(cost)))
                            .font(.system(size: 26, weight: .medium, design: .monospaced))
                            .foregroundStyle(Palette.ink)
                        Spacer(minLength: 8)
                        // The host counts requests and models over 30 days only.
                        if period == .month {
                            Text("\(cost.requestsMonth) requests").font(.system(size: 11.5)).foregroundStyle(Palette.muted)
                        }
                    }
                    if period == .month {
                        ForEach(cost.topModels, id: \.model) { entry in
                            HStack(spacing: 10) {
                                Text(entry.model).lineLimit(1).truncationMode(.middle).frame(width: 64, alignment: .leading)
                                GeometryReader { proxy in
                                    Capsule().fill(Palette.line)
                                        .overlay(alignment: .leading) {
                                            Capsule()
                                                .fill(LinearGradient(colors: [tint.a, tint.b], startPoint: .leading, endPoint: .trailing))
                                                .frame(width: max(4, proxy.size.width * entry.share))
                                        }
                                }
                                .frame(height: 5)
                                Text(KithCost.format(entry.usd)).monospacedDigit().frame(minWidth: 44, alignment: .trailing)
                            }
                            .font(.system(size: 11.5))
                            .foregroundStyle(Palette.muted)
                        }
                    }
                }
            } else {
                SheetNote(text: "No cost recorded yet")
            }
        }
    }

    private func amount(_ cost: KithCost) -> Double {
        switch period {
        case .today: cost.today
        case .week: cost.week
        case .month: cost.month
        case .all: cost.all
        }
    }

    // MARK: projects, harness

    @ViewBuilder private func projects(_ info: KithInfo) -> some View {
        if info.errors["projects"] != nil {
            CardSection("Projects") { SheetNote(text: "Couldn't read the projects") }
        } else if !info.projects.isEmpty {
            CardSection("Projects") {
                ForEach(info.projects) { project in
                    SheetRow(icon: "briefcase", title: project.id,
                             detail: [project.files == 1 ? "1 file" : "\(project.files) files",
                                      project.modified.map { kithAgo($0.date) }].compactMap { $0 }.joined(separator: " · "))
                }
            }
        }
    }

    @ViewBuilder private func harness(_ info: KithInfo) -> some View {
        // Model, effort and permission are in the bot's settings (hq-backed rows, once); the harness is what is left here.
        if let harness = info.harness {
            CardSection("Harness", footer: "Set in the agent's package") {
                SheetRow(icon: "cpu", title: harness)
            }
        }
    }

    // MARK: access

    /// The agent's "always" answers to escape cards, each with a Remove; only when there is one (the owner's own).
    @ViewBuilder private var access: some View {
        if !store.grants.isEmpty {
            CardSection("Access", footer: store.grantError.map { "Not removed: \($0)" }
                        ?? "Allowed always from a permission card. A removed one is gone from their next run.") {
                ForEach(store.grants) { grant in
                    SheetRow(icon: grant.kind == "domain" ? "globe" : "folder", title: grant.text, detail: grant.holder) {
                        if store.removingGrant == grant {
                            ProgressView().controlSize(.small)
                        } else {
                            IconButton("Remove", systemImage: "trash") { Task { await store.remove(grant) } }
                                .disabled(store.removingGrant != nil)
                        }
                    }
                }
            }
        }
    }

    // MARK: skills, tools, plugins

    @ViewBuilder private func capabilities(_ info: KithInfo) -> some View {
        let errors = ["skills", "tools", "mcp_servers", "plugins"].contains { info.errors[$0] != nil }
        if errors || !(info.skills.isEmpty && info.tools.isEmpty && info.mcpServers.isEmpty && info.plugins.isEmpty) {
            CardSection("Skills, tools, plugins") {
                if info.errors["skills"] != nil { SheetNote(text: "Couldn't read skills") }
                else if !info.skills.isEmpty {
                    SheetCountRow(icon: "sparkles", title: "Skills", count: info.skills.count) {
                        ForEach(info.skills) { skill in
                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 6) {
                                    Text(skill.name).font(.system(size: 13, weight: .medium)).foregroundStyle(Palette.ink)
                                    if skill.source != "package" {
                                        Text(skill.source).font(.system(size: 10.5)).foregroundStyle(Palette.muted)
                                            .padding(.horizontal, 6).padding(.vertical, 1)
                                            .background(Palette.panel, in: Capsule())
                                    }
                                }
                                if !skill.description.isEmpty {
                                    Text(skill.description).font(.system(size: 11.5)).foregroundStyle(Palette.muted)
                                }
                            }
                        }
                    }
                }
                if info.errors["tools"] != nil { SheetNote(text: "Couldn't read tools") }
                else if !info.tools.isEmpty {
                    SheetCountRow(icon: "wrench.and.screwdriver", title: "Tools", count: info.tools.count) {
                        ForEach(info.tools, id: \.self) { Text($0).font(KithText.code.font).foregroundStyle(Palette.ink) }
                    }
                }
                if info.errors["mcp_servers"] != nil { SheetNote(text: "Couldn't read mcp servers") }
                else if !info.mcpServers.isEmpty {
                    SheetCountRow(icon: "server.rack", title: "MCP servers", count: info.mcpServers.count) {
                        ForEach(info.mcpServers) { server in
                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 6) {
                                    Text(server.name).font(.system(size: 13, weight: .medium)).foregroundStyle(Palette.ink)
                                    if let kind = server.kind { Text(kind).font(.system(size: 11.5)).foregroundStyle(Palette.muted) }
                                }
                                if let target = server.target {
                                    Text(target).font(KithText.code.font).foregroundStyle(Palette.muted).lineLimit(2)
                                }
                            }
                        }
                    }
                }
                if info.errors["plugins"] != nil { SheetNote(text: "Couldn't read plugins") }
                else if !info.plugins.isEmpty {
                    SheetCountRow(icon: "puzzlepiece.extension", title: "Plugins", count: info.plugins.count) {
                        ForEach(info.plugins, id: \.self) { Text($0).font(KithText.code.font).foregroundStyle(Palette.ink) }
                    }
                }
            }
        }
    }
}

/// The role prompt: a row that opens the full text in place.
struct KithInstructionsRow: View {
    let text: String
    @State private var open = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { open.toggle() }
            } label: {
                SheetRow(icon: "doc.text", title: "Instructions", detail: open ? nil : String(text.prefix(90)), open: open)
            }
            .buttonStyle(.plain)
            .accessibilityValue(open ? "Expanded" : "Collapsed")
            if open { MarkdownText(text).padding(.leading, 29) }
        }
    }
}

/// One kith schedule as a read-only row: the task, when it runs, and how the last run went.
struct KithScheduleRow: View {
    let job: KithSchedule
    @State private var open = false

    var body: some View {
        Button { withAnimation(Motion.layout) { open.toggle() } } label: {
            SheetRow(icon: "clock", title: job.task, detail: detail, chevron: true)
                .lineLimit(open ? nil : 1)
        }
        .buttonStyle(.plain)
    }

    private var detail: String {
        var parts = ["kith schedule", job.plainCron + (job.paused ? " (paused)" : "")]
        if job.isRunning { parts.append("running") } else if let last = job.lastRun { parts.append("last run \(kithAgo(last.date))") } else { parts.append("never run") }
        if !job.paused, let next = job.nextRun, next.date > .now { parts.append("next in \(RelativeTime.until(next.date))") }
        return parts.joined(separator: " · ") + " · read-only"
    }
}
