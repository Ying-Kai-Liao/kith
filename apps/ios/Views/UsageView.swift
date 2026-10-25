import KithKit
import KithUI
import SwiftUI

/// Usage limits (opened from the State tab): one grouped card per provider, a row per limit with its own bar; then, for
/// the computer's owner, what agents and your own sessions cost at API prices and each agent's monthly limit.
struct UsageView: View {
    @Environment(BotStore.self) private var model
    @State private var collapsed: Set<String> = []

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(spacing: 22) {
                    ForEach(model.usage.providers) { provider in
                        ProviderCard(provider: provider, collapsed: Binding(
                            get: { collapsed.contains(provider.id) },
                            set: { if $0 { collapsed.insert(provider.id) } else { collapsed.remove(provider.id) } }
                        ))
                    }
                    if model.usage.providers.isEmpty {
                        VStack(spacing: 8) {
                            Image(systemName: "chart.bar").font(.system(size: 40)).foregroundStyle(Palette.tertiary)
                            Text(model.isOwner ? "No plan limits yet." : "No usage information yet.").font(.title3.weight(.semibold)).foregroundStyle(Palette.text)
                            Text("Your computer reads the limits from Claude Code and Codex.")
                                .font(.subheadline).foregroundStyle(Palette.secondary)
                        }
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: .infinity)
                        .padding(.top, model.isOwner ? 20 : 60)
                    }
                    SpendSection(store: model).id("spend")
                }
                .padding(16)
            }
            .task {
                guard let to = DemoScreens.open("USAGE_SCROLL") else { return }
                try? await Task.sleep(for: .seconds(3))
                proxy.scrollTo(to, anchor: .top)
            }
        }
        .background(Palette.background)
        .refreshable { await model.refreshUsage() }
    }
}

private struct ProviderCard: View {
    let provider: UsageProvider
    @Binding var collapsed: Bool
    @AppStorage(SharedStore.usageIconStyleKey, store: UserDefaults(suiteName: SharedStore.appGroup))
    private var usageIconStyle = UsageIconStyle.character.rawValue
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                ProviderMascot(provider, size: 18, style: UsageIconStyle(rawValue: usageIconStyle) ?? .character)
                Text(provider.name.uppercased())
                    .font(.system(size: 10.5, weight: .bold))
                    .tracking(10.5 * 0.08)
                    .foregroundStyle(Palette.muted)
                    .accessibilityAddTraits(.isHeader)
                Spacer(minLength: 0)
                IconButton(collapsed ? "Expand" : "Collapse", systemImage: "chevron.up") {
                    withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { collapsed.toggle() }
                }
                .rotationEffect(.degrees(collapsed ? 180 : 0))
            }
            .padding(.leading, 2)

            VStack(spacing: 0) {
                if collapsed {
                    if let top = provider.tightest { limitRow(top.label, top.percent, top.resetsShort()) }
                } else {
                    ForEach(provider.windows) { w in limitRow(w.label, w.percent, w.resetsShort()) }
                    infoRow("Source", source)
                    infoRow("Updated", Date(milliseconds: provider.updatedAt).formatted(date: .abbreviated, time: .shortened))
                }
            }
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
    }

    private var source: String {
        switch provider.source {
        case "claude": "Claude Code"
        case "statusline": "Claude Code status line"
        case "agent": "A running bot"
        case "sessions": "Codex sessions"
        default: provider.source
        }
    }

    /// The name, "NN% · resets …", and a 6 pt bar.
    private func limitRow(_ label: String, _ percent: Double, _ reset: String?) -> some View {
        let pct = Int(percent.rounded())
        return VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(label).font(.subheadline.weight(.medium)).foregroundStyle(Palette.text)
                Spacer(minLength: 8)
                Text(reset.map { "\(pct)% · \($0)" } ?? "\(pct)%")
                    .font(.footnote.monospacedDigit()).foregroundStyle(Palette.secondary)
            }
            Capsule().fill(Palette.accentDim).frame(height: 6)
                .overlay(alignment: .leading) {
                    GeometryReader { geo in
                        Capsule().fill(Palette.text).frame(width: geo.size.width * min(max(percent, 0), 100) / 100)
                    }
                }
        }
        .padding(.horizontal, 14).padding(.vertical, 12)
        .accessibilityElement(children: .combine)
    }

    private func infoRow(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label).foregroundStyle(Palette.secondary)
            Spacer()
            Text(value).foregroundStyle(Palette.text).monospacedDigit()
        }
        .font(.footnote)
        .padding(.horizontal, 14).padding(.vertical, 10)
    }
}
