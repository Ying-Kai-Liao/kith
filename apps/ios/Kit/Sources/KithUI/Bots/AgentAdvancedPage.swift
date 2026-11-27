import KithKit
import SwiftUI

/// The agent page's Advanced page (mock agadv:): how it thinks, where it works, accounts and keys, routines, and what it
/// is made of. Model, effort, engine and call model are written through to kith and show kith's answer, not the pick.
/// Skills, tools and MCP servers are view only: changes go through builder.
struct AgentAdvancedPage: View {
    let name: String
    @Binding var draft: BotDraft
    let kith: KithInfoStore?
    let access: AccessStore?
    let shown: BotSections
    let open: (AgentSubpage) -> Void
    /// Puts a draft in builder's chat; nil when this computer has no builder.
    let askBuilder: ((String) -> Void)?
    @Environment(BotStore.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            if let kith {
                CardSection("How it thinks", footer: model.isOffline ? "\(model.hostName) is offline, so these can't change now." : nil) {
                    HqSettingsRows(store: kith, showsPermission: false)
                }
                .disabled(model.isOffline)
            } else {
                SheetNote(text: "Engine, model and effort come from Kith on the computer.")
            }
            BotSettingsForm(draft: $draft, error: nil, scrolls: false, part: .workspace, shown: shown, kith: kith)
            CardSection("Accounts and keys") {
                Button { open(.keys) } label: {
                    SheetRow(icon: "key", title: "Keys", value: keysValue, chevron: true)
                }
                .buttonStyle(.plain)
                .disabled(access?.available == false)
            }
            CardSection("Activity") {
                SwitchRow("Show summary on row", detail: "The list shows the latest thread's summary instead of the last message, once one is written.",
                          isOn: Binding(get: { draft.rowShowsSummary ?? false }, set: { draft.rowShowsSummary = $0 }))
                Button { open(.routines) } label: {
                    SheetRow(icon: "clock", title: "Routines", value: routinesValue, chevron: true)
                }
                .buttonStyle(.plain)
            }
            if let info = kith?.info {
                KithMadeOf(info: info)
                CardSection(footer: "Skills, tools and connectors change through builder. It shows the change and waits for your Allow.") {
                    Button { askBuilder?("I want to change \(name)'s skills, tools or connectors: ") } label: {
                        SheetRow(icon: "sparkles", title: "Ask builder", detail: askBuilder == nil ? "builder isn't on this computer" : nil,
                                 chevron: true, accent: askBuilder != nil)
                    }
                    .buttonStyle(.plain)
                    .disabled(askBuilder == nil)
                }
                if let agent = model.bots[draft.id ?? ""]?.hqAgent, let kith {
                    HStack { KithIdentityLine(agent: agent, store: kith); Spacer(minLength: 0) }
                }
            }
        }
    }

    private var keysValue: String {
        guard let access else { return "" }
        if case .loaded = access.keys { return access.held.isEmpty ? "None" : "\(access.held.count)" }
        return ""
    }

    private var routinesValue: String {
        let count = kith?.info?.schedules.count ?? 0
        return count == 0 ? "" : "\(count) from kith"
    }
}

/// The agent's Keys page (mock agkeys:): only the keys this agent holds, with Remove. The global Keys page is unchanged.
struct AgentKeysPage: View {
    let store: AccessStore
    @Environment(BotStore.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            switch store.keys {
            case .hidden: SheetNote(text: "This computer doesn't list keys for you.")
            case .loading: CardSection { SheetNote(text: "Loading…") }
            case .failed(let message): CardSection { SheetNote(text: "Couldn't read the keys: \(message)", danger: true) }
            case .loaded:
                if store.held.isEmpty {
                    CardSection(footer: "When it needs one, it asks you in its chat.") { SheetNote(text: "No keys yet") }
                } else {
                    CardSection(footer: "Remove takes this agent off the key. The key stays for the others.") {
                        ForEach(store.held) { key in
                            SheetRow(icon: "key", title: AccessTab.accountTitle(key), detail: AccessTab.keyDetail(key) { kithAgo($0, now: $1) }) {
                                if store.removing.contains("key/" + key.profile) {
                                    ProgressView().controlSize(.small)
                                } else {
                                    IconButton("Remove", systemImage: "trash") { Task { await store.revoke(key) } }
                                        .disabled(model.isOffline)
                                }
                            }
                        }
                    }
                }
            }
            if let error = store.removeError {
                Text("Not removed: \(error)").font(.footnote).foregroundStyle(Palette.danger)
            }
        }
    }
}
