import KithKit
import SwiftUI

/// One line on an agent's Permissions page.
struct PermissionLine: Identifiable, Equatable {
    let id: String
    let text: String
    let detail: String?
    /// Where the rule comes from ("Comes with it", "You allowed it"); nil until the host sends `agentRules` with source tags.
    var source: String?
    /// The owner can take it away from here: today only what "Always allow" remembered.
    let removable: Bool
}

/// A titled group of lines: "Allow without asking", later "Always ask" and "Never".
struct PermissionGroup: Identifiable, Equatable {
    let id: String
    let title: String
    let footer: String?
    let emptyNote: String
    let lines: [PermissionLine]
}

/// What the Permissions page lists. Today that is the owner's "Always allow" rules (`allowedRules`). The page draws
/// whatever groups this returns, so the later PR that wires the host's `agentRules` (package allow / ask / deny with
/// source tags) adds its groups here and leaves the view alone.
enum AgentPermissions {
    static func groups(allowed: AllowedList, ago: (Date, Date) -> String, now: Date = .now) -> [PermissionGroup] {
        [PermissionGroup(
            id: "allowed", title: "Allow without asking",
            footer: allowed.allowed.isEmpty ? nil : "A removed rule is gone at this agent's next run.",
            emptyNote: "Nothing yet. “Always allow” on a blocked action lands here.",
            lines: allowed.allowed.map {
                PermissionLine(id: "allowed/" + $0.id, text: $0.text(), detail: $0.detail(now: now, ago: ago), source: nil, removable: true)
            })]
    }

    /// The merged rules from `agentRules`, grouped Allow, Ask, Never in the host's order. Only a rule from
    /// allowed.json (it carries an id) can be removed; the package's own rules are read-only.
    static func groups(rules: [HostContract.AgentRule]) -> [PermissionGroup] {
        let lists: [(key: String, title: String, empty: String)] = [
            ("allow", "Allow without asking", "Nothing yet. “Always allow” on a blocked action lands here."),
            ("ask", "Always ask", "Nothing here."),
            ("deny", "Never", "Nothing here."),
        ]
        return lists.map { list in
            let own = rules.filter { $0.list == list.key }
            let lines = own.enumerated().map { index, rule -> PermissionLine in
                let owned = rule.source == "allowed.json" && rule.id != nil
                return PermissionLine(id: owned ? "allowed/" + rule.id! : "\(list.key)/\(index)", text: rule.rule, detail: nil,
                                      source: owned ? "You allowed it" : "Comes with it", removable: owned)
            }
            return PermissionGroup(id: list.key, title: list.title,
                                   footer: list.key == "allow" && lines.contains { $0.removable } ? "A removed rule is gone at this agent's next run." : nil,
                                   emptyNote: list.empty, lines: lines)
        }
    }

    /// The rule id a removable line stands for.
    static func ruleId(_ line: PermissionLine) -> String { String(line.id.dropFirst("allowed/".count)) }

    /// "Asks before: Default" on the agent page, from kith's answer.
    static func modeLabel(_ info: KithInfo?) -> String? {
        info.map { HqSetting.permissionLabel($0.permissionMode ?? "auto") }
    }
}

/// The Permissions page: read-only apart from the mode (a host setter today) and Remove on an owner's own rule.
struct AgentPermissionsPage: View {
    let name: String
    let store: AccessStore
    let kith: KithInfoStore?
    /// Opens the agent's chat with a draft in its box; nothing is sent.
    let ask: (String) -> Void
    @Environment(BotStore.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            AccessTabView(store: store, kith: kith).asksBefore
            if let rules = store.rules {
                ForEach(AgentPermissions.groups(rules: rules)) { group in
                    CardSection(group.title, footer: group.footer) {
                        if group.lines.isEmpty { SheetNote(text: group.emptyNote) }
                        ForEach(group.lines) { line in
                            SheetRow(icon: Self.icon(group.id), title: line.text, detail: line.source) {
                                if line.removable { remove(line) }
                            }
                        }
                    }
                }
            } else {
            switch store.allowed {
            case .hidden: EmptyView()
            case .loading: CardSection("Allow without asking") { SheetNote(text: "Loading…") }
            case .failed(let message):
                CardSection("Allow without asking") { SheetNote(text: "Couldn't read the allowed list: \(message)", danger: true) }
            case .loaded(let list):
                ForEach(AgentPermissions.groups(allowed: list, ago: { kithAgo($0, now: $1) })) { group in
                    CardSection(group.title, footer: group.footer) {
                        if group.lines.isEmpty && list.error == nil { SheetNote(text: group.emptyNote) }
                        ForEach(group.lines) { line in
                            SheetRow(icon: "checkmark.shield", title: line.text, detail: [line.source, line.detail].compactMap { $0 }.joined(separator: " · ")) {
                                if line.removable { remove(line) }
                            }
                        }
                        if let error = list.error {
                            SheetNote(text: "Some rules could not be read: \(error)", danger: true)
                        }
                    }
                }
            }
            }
            if let error = store.removeError {
                Text("Not removed: \(error)").font(.footnote).foregroundStyle(Palette.danger)
            }
            CardSection(footer: "Say it in your words. \(name) shows the change and waits for your Allow.") {
                Button { ask("I want to change what you're allowed to do: ") } label: {
                    SheetRow(icon: "sparkles", title: "Ask \(name) to change these", chevron: true, accent: true)
                }
                .buttonStyle(.plain)
            }
        }
    }

    private static func icon(_ group: String) -> String {
        switch group {
        case "deny": "nosign"
        case "ask": "questionmark.circle"
        default: "checkmark.shield"
        }
    }

    @ViewBuilder private func remove(_ line: PermissionLine) -> some View {
        let busy = store.removing.contains(line.id)
        if busy {
            ProgressView().controlSize(.small)
        } else {
            IconButton("Remove", systemImage: "trash") {
                let id = AgentPermissions.ruleId(line)
                Task { await store.remove(ruleId: id) }
            }
            .disabled(model.isOffline)
        }
    }
}
