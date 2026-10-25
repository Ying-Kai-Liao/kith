import KithKit
import KithUI
import SwiftUI

/// Settings › Keys, the owner's only: the service keys saved on this computer, grouped by service, with the agents
/// that hold each one and when it was last used. "Remove access" takes one agent off a key. The screen never
/// shows or asks for a value.
struct KeysSection: View {
    let store: BotStore
    @State private var keys: KeysStore
    @State private var showing = false

    init(store: BotStore) {
        self.store = store
        _keys = State(initialValue: KeysStore(store: store))
        _showing = State(initialValue: DemoScreens.name == "keys-list") // the screenshot demo opens the list
    }

    var body: some View {
        CardSection("Keys", footer: "Service keys your agents use, kept in the Keychain on \(store.hostName). Agents never see them.") {
            Button { showing = true } label: {
                LinkRow { Label("Keys", systemImage: "key") }
            }
            .buttonStyle(.plain)
        }
        .kithSheet(isPresented: $showing) { KeysView(keys: keys, hostName: store.hostName) }
    }
}

struct KeysView: View {
    let keys: KeysStore
    let hostName: String
    @State private var removing: Removal?

    private struct Removal: Identifiable {
        let key: KeyRecord
        let agent: String
        var id: String { key.profile + "/" + agent }
    }

    var body: some View {
        CardForm {
            if let error = keys.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            if keys.loaded, keys.keys.isEmpty {
                CardSection("Keys", footer: "When an agent needs a service key, it asks you in its chat. Saved keys show here.") {
                    Text("No keys yet").foregroundStyle(Palette.secondary)
                }
            }
            ForEach(keys.groups, id: \.service) { group in
                CardSection(group.service.capitalized) {
                    ForEach(group.keys) { key in
                        VStack(alignment: .leading, spacing: 6) {
                            Text(key.account).font(.body.weight(.semibold)).foregroundStyle(Palette.ink)
                            Text(lastUsed(key)).font(.subheadline).foregroundStyle(Palette.secondary)
                            if key.agents.isEmpty {
                                Text("No agent has it").font(.subheadline).foregroundStyle(Palette.tertiary)
                            }
                            ForEach(key.agents, id: \.self) { agent in
                                HStack {
                                    Text(agent).font(.subheadline).foregroundStyle(Palette.ink)
                                    Spacer()
                                    Button("Remove access") { removing = Removal(key: key, agent: agent) }
                                        .font(.subheadline).foregroundStyle(Palette.danger)
                                        .disabled(keys.removing.contains(key.profile + "/" + agent))
                                }
                                .frame(minHeight: 36)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader("Keys") }
        .refreshable { await keys.load() }
        .task { await keys.load() }
        .kithDialog("Remove access?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
                    message: removing.map { "\($0.agent) can no longer use the \($0.key.service) key \($0.key.account). The key stays on \(hostName)." }) {
            guard let removal = removing else { return [] }
            return [DialogAction("Remove access", destructive: true) { Task { await keys.remove(agent: removal.agent, from: removal.key) } }]
        }
    }

    private func lastUsed(_ key: KeyRecord) -> String {
        guard let date = key.lastUsedDate else { return "Never used" }
        return "Last used " + RelativeTime.day(date)
    }
}
