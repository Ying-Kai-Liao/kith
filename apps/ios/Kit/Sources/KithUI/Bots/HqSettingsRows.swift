import KithKit
import SwiftUI

/// An hq bot's permission, call model, harness, model and effort, once. hq holds them: each is written through
/// (`kith <agent> set …`) and shown as hq reports them, and a refusal shows the CLI's own words. Fast mode has no kith setting, so it isn't offered.
struct HqSettingsRows: View {
    let store: KithInfoStore
    /// False once the Access tab shows the permission mode.
    var showsPermission = true
    @Environment(BotStore.self) private var botStore
    /// The harness's models, for the Model row; empty until (or unless) the computer lists them.
    @State private var models: [AgentModels.Model] = []

    var body: some View {
        ForEach(HqSetting.rows(store.info).filter { showsPermission || $0.key != .permission }, id: \.key) { row in
            ValueRow(row.label, detail: row.hint) {
                if row.editable {
                    ChoicePicker(selection: binding(row), options: choices(row), detail: { row.key == .permission ? HqSetting.permissionDetail($0) : nil })
                        .disabled(isSaving(row.key))
                } else {
                    Text(row.value).foregroundStyle(Palette.secondary).lineLimit(1)
                }
            }
        }
        .task(id: store.info?.harness) { await loadModels() }
        if store.info == nil, !store.isLoading {
            // Kith couldn't answer (or the host predates it): say so and offer a retry, never the host's own pickers.
            VStack(alignment: .leading, spacing: 6) {
                Text(store.failure ?? "Couldn't reach Kith for this agent's settings.").font(.footnote).foregroundStyle(Palette.danger)
                Button("Retry") { Task { await store.load(refresh: true) } }.font(.footnote)
            }
        }
        if let error = store.callModelError {
            Text("Call model not changed: \(error)").font(.footnote).foregroundStyle(Palette.danger)
        }
        if let error = store.settingError {
            Text("\(error.setting.prefix(1).uppercased() + error.setting.dropFirst()) not changed: \(error.text)")
                .font(.footnote).foregroundStyle(Palette.danger)
        }
        if showsPermission, let error = store.permissionError {
            Text("Permission not changed: \(error)").font(.footnote).foregroundStyle(Palette.danger)
        }
    }

    private func isSaving(_ key: HqSetting.Key) -> Bool {
        switch key {
        case .permission: store.savingPermission
        case .callModel: store.savingCallModel
        case .harness, .model, .effort: store.savingSetting != nil
        case .fastMode: false
        }
    }

    private func binding(_ row: HqSetting) -> Binding<String> {
        Binding(
            get: { row.value },
            set: { pick in
                guard pick != row.value else { return }
                Task {
                    switch row.key {
                    case .callModel: await store.setCallModel(pick)
                    case .harness: await store.setSetting("harness", pick)
                    case .model: await store.setSetting("model", pick)
                    case .effort: await store.setSetting("effort", pick)
                    case .permission, .fastMode: await store.setPermission(pick)
                    }
                }
            })
    }

    private func choices(_ row: HqSetting) -> [(id: String, label: String)] {
        switch row.key {
        case .callModel: callModels(including: row.value)
        case .harness: withCurrent(HqSetting.harnesses, row.value)
        case .effort: withCurrent(HqSetting.effortLevels, row.value)
        case .model:
            withCurrent([(id: "default", label: "Agent's default")] + models.map { (id: $0.id, label: $0.name.isEmpty ? $0.id : $0.name) }, row.value)
        default: options(including: row.value)
        }
    }

    private func withCurrent(_ known: [(id: String, label: String)], _ current: String) -> [(id: String, label: String)] {
        known.contains { $0.id == current } ? known : known + [(id: current, label: current)]
    }

    /// The agent's models on its harness, as the computer lists them; a failure leaves the Model row with Default and the current one.
    private func loadModels() async {
        guard let harness = store.info?.harness ?? (store.info == nil ? nil : "claude"), let client = botStore.client else { return }
        models = (try? await client.agentModels(harness).models) ?? []
    }

    private func callModels(including current: String) -> [(id: String, label: String)] {
        let known = HqSetting.callModels(defaultIs: store.info?.callModelDefault)
        return known.contains { $0.id == current } ? known : known + [(id: current, label: HqSetting.callModelLabel(current))]
    }

    /// hq's modes, plus the current one when this app doesn't know it.
    private func options(including current: String) -> [(id: String, label: String)] {
        let known = HqSetting.permissionModes
        return known.contains { $0.id == current } ? known : known + [(id: current, label: HqSetting.permissionLabel(current))]
    }
}
