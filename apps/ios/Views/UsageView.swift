import KithKit
import KithUI
import SwiftUI

/// Usage (You > Usage): plan meters first, one grouped card per provider with a row per limit; then, for the
/// computer's owner, this calendar month's cost at API prices, Agents | People.
struct UsageView: View {
    @Environment(BotStore.self) private var model
    @State private var collapsed: Set<String> = []
    /// Each account's windows (host fact 0.1); nil until the host answers, or when it can't (older host, not the owner).
    @State private var harness: [HarnessAccount]?
    private var meters: PlanMeters { PlanMeters(model.usage, accounts: harness) }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(spacing: 22) {
                    // Plan meters first, one set per account.
                    ForEach(meters.accounts) { account in
                        ProviderCard(provider: account.provider, readAt: account.readAt, collapsed: Binding(
                            get: { collapsed.contains(account.id) },
                            set: { if $0 { collapsed.insert(account.id) } else { collapsed.remove(account.id) } }
                        ))
                    }
                    if meters.accounts.isEmpty {
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
        .task { await loadAccounts() }
        .refreshable {
            await model.refreshUsage()
            await loadAccounts()
        }
    }
}

extension UsageView {
    /// An answer that fails keeps what is shown: the provider sets stand in until the host reads the accounts.
    private func loadAccounts() async {
        if let demo = DemoScreens.meterAccounts { harness = demo; return }
        guard model.isOwner, let client = model.client else { return }
        if let answer = try? await client.accounts() { harness = answer.accounts }
    }
}

private struct ProviderCard: View {
    let provider: UsageProvider
    /// When the host read this account's windows (per-account meters); nil for the provider-wide set.
    var readAt: Date?
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
                    // One footer for every provider: when it was read, always relative.
                    infoRow("Updated", PlanMeters.readLabel(readAt ?? Date(milliseconds: provider.updatedAt)))
                }
            }
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
    }

    /// The name, "NN% · resets …", and a 6 pt bar.
    private func limitRow(_ label: String, _ percent: Double, _ reset: String?) -> some View {
        let pct = Int(percent.rounded())
        // Red always comes with a word (DESIGN.md): at 90% or more the bar turns red and the line says so.
        let nearly = pct >= 90
        return VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(label).font(.subheadline.weight(.medium)).foregroundStyle(Palette.text)
                Spacer(minLength: 8)
                Text([ "\(pct)%", nearly ? "almost used up" : nil, reset ].compactMap { $0 }.joined(separator: " · "))
                    .font(.footnote.monospacedDigit()).foregroundStyle(nearly ? Palette.danger : Palette.secondary)
            }
            Capsule().fill(Palette.accentDim).frame(height: 6)
                .overlay(alignment: .leading) {
                    GeometryReader { geo in
                        Capsule().fill(nearly ? Palette.danger : Palette.text).frame(width: geo.size.width * min(max(percent, 0), 100) / 100)
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
