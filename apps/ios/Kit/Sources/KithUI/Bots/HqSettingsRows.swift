import KithKit
import SwiftUI

/// An hq bot's permission, call model, model, effort and Fast mode, once. hq holds them: the permission and the call model
/// are written through to hq and shown as hq reports them; the others hq has no setting for, so they are read-only and say "Set in Kith".
struct HqSettingsRows: View {
    let store: KithInfoStore
    /// False once the Access tab shows the permission mode.
    var showsPermission = true

    var body: some View {
        ForEach(HqSetting.rows(store.info).filter { showsPermission || $0.key != .permission }, id: \.key) { row in
            ValueRow(row.label, detail: row.hint) {
                if row.editable {
                    ChoicePicker(
                        selection: Binding(
                            get: { row.value },
                            set: { pick in
                                guard pick != row.value else { return }
                                Task {
                                    if row.key == .callModel { await store.setCallModel(pick) } else { await store.setPermission(pick) }
                                }
                            }),
                        options: row.key == .callModel ? callModels(including: row.value) : options(including: row.value),
                        detail: { row.key == .callModel ? nil : HqSetting.permissionDetail($0) })
                } else {
                    Text(row.value).foregroundStyle(Palette.secondary).lineLimit(1)
                }
            }
        }
        if let error = store.callModelError {
            Text("Call model not changed: \(error)").font(.footnote).foregroundStyle(Palette.danger)
        }
        if showsPermission, let error = store.permissionError {
            Text("Permission not changed: \(error)").font(.footnote).foregroundStyle(Palette.danger)
        }
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
