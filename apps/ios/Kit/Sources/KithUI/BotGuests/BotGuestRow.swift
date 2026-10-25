import KithKit
import SwiftUI

/// One bot from outside in a list: the bot's own avatar (not a living bot's), its name and provider, a line by state, and
/// the actions that state has.
public struct BotGuestRow: View {
    let bot: BotGuest
    let viewer: BotViewer
    let store: BotStore
    let indent: CGFloat
    let open: (BotGuest) -> Void
    let newLink: (BotGuest) -> Void

    public init(bot: BotGuest, viewer: BotViewer, store: BotStore, indent: CGFloat = 0,
                open: @escaping (BotGuest) -> Void, newLink: @escaping (BotGuest) -> Void) {
        self.bot = bot
        self.viewer = viewer
        self.store = store
        self.indent = indent
        self.open = open
        self.newLink = newLink
    }

    private var model: BotGuestRowModel { BotGuestRowModel(bot, viewer: viewer, ownerName: store.ownerCalled) }
    private var guests: BotGuestStore { store.botGuests }

    public var body: some View {
        let model = model
        HStack(spacing: 12) {
            BotGuestAvatar(provider: bot.provider)
            VStack(alignment: .leading, spacing: 2) {
                (Text(model.title).font(.body.weight(.semibold)).foregroundStyle(Palette.text)
                    + Text(" · \(model.provider)").font(.subheadline).foregroundStyle(Palette.secondary))
                    .lineLimit(1)
                HStack(spacing: 6) {
                    if model.needsYou { Circle().fill(Palette.ink).frame(width: 6, height: 6) }
                    Text(model.line)
                        .font(.subheadline)
                        .foregroundStyle(model.needsYou ? Palette.ink : Palette.secondary)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: 8)
            ForEach(model.actions, id: \.self) { action in actionView(action) }
        }
        .padding(.leading, indent)
        .contentShape(Rectangle())
        .onTapGesture { if model.actions.contains(.chevron) { open(bot) } }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(model.title), \(model.provider). \(model.line)")
    }

    @ViewBuilder private func actionView(_ action: BotGuestRowModel.Action) -> some View {
        switch action {
        case .cancelInvite:
            Button("Cancel invite") { Task { await guests.remove(bot.botId) } }
                .buttonStyle(.plain).font(.subheadline).foregroundStyle(Palette.danger)
                .disabled(guests.busy)
        case .newLink:
            Button("New link") { newLink(bot) }
                .buttonStyle(.plain).font(.subheadline).foregroundStyle(Palette.accent)
                .disabled(guests.busy)
        case .deny:
            Button("Deny") { Task { await guests.decide(bot, option: "deny") } }
                .buttonStyle(.plain).font(.subheadline.weight(.semibold)).foregroundStyle(Palette.danger)
                .disabled(guests.deciding[bot.botId] != nil)
        case .allow:
            Button("Allow") { Task { await guests.decide(bot, option: "allow") } }
                .buttonStyle(.plain).font(.subheadline.weight(.semibold)).foregroundStyle(Palette.accent)
                .disabled(guests.deciding[bot.botId] != nil)
        case .chevron:
            Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(Palette.tertiary)
        case .spinner:
            Spinner(size: 16)
        }
    }
}

/// "Ended (3)": denied and removed bots, folded until opened.
public struct EndedBots: View {
    let bots: [BotGuest]
    let viewer: BotViewer
    let store: BotStore
    let indent: CGFloat
    @State private var shown = false

    public init(bots: [BotGuest], viewer: BotViewer, store: BotStore, indent: CGFloat = 0) {
        self.bots = bots
        self.viewer = viewer
        self.store = store
        self.indent = indent
    }

    public var body: some View {
        if !bots.isEmpty {
            Button { withAnimation(Motion.layout) { shown.toggle() } } label: {
                HStack {
                    Text("Ended (\(bots.count))").font(.subheadline).foregroundStyle(Palette.secondary)
                    Spacer()
                    Image(systemName: "chevron.down")
                        .font(.footnote.weight(.semibold)).foregroundStyle(Palette.tertiary)
                        .rotationEffect(.degrees(shown ? 180 : 0))
                }
                .padding(.leading, indent)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityValue(shown ? "Shown" : "Hidden")
            if shown {
                ForEach(bots) { bot in
                    BotGuestRow(bot: bot, viewer: viewer, store: store, indent: indent, open: { _ in }, newLink: { _ in })
                }
            }
        }
    }
}
