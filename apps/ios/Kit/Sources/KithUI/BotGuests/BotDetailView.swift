import KithKit
import SwiftUI

/// One bot from outside: what it can ask, how it stands, its conversations (the computer's owner only), and the way to
/// remove it. What it can ask is fixed: to change it, remove the bot and invite it again.
public struct BotDetailView: View {
    let botId: String
    let viewer: BotViewer
    let store: BotStore
    @Environment(\.dismissModal) private var dismiss
    @Environment(AccountStore.self) private var accounts: AccountStore?
    @State private var confirmRemove: Bool
    /// The last row seen, so the sheet still reads after the bot leaves the list.
    @State private var last: BotGuest

    /// `askRemove` opens the remove dialog at once (screenshots).
    public init(bot: BotGuest, viewer: BotViewer, store: BotStore, askRemove: Bool = false) {
        _confirmRemove = State(initialValue: askRemove)
        botId = bot.botId
        self.viewer = viewer
        self.store = store
        _last = State(initialValue: bot)
    }

    private var guests: BotGuestStore { store.botGuests }
    private var bot: BotGuest { guests.bot(botId) ?? last }
    private var ownerName: String { store.ownerCalled }

    public var body: some View {
        let bot = bot
        CardForm {
            CardSection("Can ask", footer: "To change this, remove \(bot.name) and invite it again.") {
                ForEach(bot.agents, id: \.self) { agent in
                    HStack(spacing: 12) {
                        agentAvatar(agent)
                        Text(agent).foregroundStyle(Palette.text)
                    }
                }
            }
            CardSection("Status", footer: BotGuestRowModel.limits(bot)) {
                ValueRow("State", value: stateWord(bot))
                if let joined = bot.joined {
                    ValueRow("Joined", value: joined.formatted(date: .abbreviated, time: .shortened))
                }
                if let seen = bot.lastSeen {
                    ValueRow("Last asked", value: seen.formatted(date: .abbreviated, time: .shortened))
                }
                if let by = BotGuestRowModel.allowedBy(bot, viewer: viewer, ownerName: ownerName) {
                    ValueRow("Allowed by", value: by)
                }
            }
            if viewer == .owner, !bot.conversations.isEmpty {
                CardSection("Conversations") {
                    ForEach(bot.conversations) { conversation in conversationRow(conversation) }
                }
            }
            if let error = guests.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            if !bot.state.isEnded {
                CardSection {
                    Button { confirmRemove = true } label: {
                        Label("Remove \(bot.name)", systemImage: "trash")
                            .foregroundStyle(Palette.danger)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(guests.busy)
                }
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) {
            ModalHeader(title: {
                VStack(alignment: .leading, spacing: 1) {
                    Text(bot.name)
                    Text(subtitle(bot)).font(.caption).fontWeight(.regular).foregroundStyle(Palette.secondary)
                }
            }, trailing: { EmptyView() })
        }
        .onChange(of: guests.bot(botId)) { _, new in if let new { last = new } }
        .kithDialog("Remove \(bot.name)?", isPresented: $confirmRemove, message: removeMessage(bot)) {
            [DialogAction("Remove", destructive: true) {
                Task { if await guests.remove(botId) { dismiss() } }
            }]
        }
    }

    private func stateWord(_ bot: BotGuest) -> String {
        switch bot.state {
        case .invited: "Invited"
        case .joining: "Joining"
        case .pending: "Waiting for an answer"
        case .active: bot.connected ? "Online now" : "Active"
        case .denied: "Denied"
        case .revoked: "Removed"
        case .unknown: "Unknown"
        }
    }

    private func subtitle(_ bot: BotGuest) -> String {
        if viewer == .owner, bot.ownerPerson != nil {
            return "\(bot.ownerName ?? "Someone")'s \(bot.providerTitle) bot"
        }
        return "\(bot.providerTitle) bot"
    }

    private func removeMessage(_ bot: BotGuest) -> String {
        var text = "\(bot.name) can't ask your agents any more, and anything it's running stops. Its conversations stay here."
        if viewer == .owner, bot.ownerPerson != nil {
            text += bot.ownerName.map { " \($0) keeps their own access." } ?? " They keep their own access."
        }
        return text
    }

    @ViewBuilder private func agentAvatar(_ agent: String) -> some View {
        if let room = store.bots.values.first(where: { $0.hqAgent == agent }) {
            CharacterAvatar(bot: room, size: 24, animated: false)
        } else {
            BotGlyph()
        }
    }

    @ViewBuilder private func conversationRow(_ conversation: BotGuestConversation) -> some View {
        let opens = BotGuestRowModel.opens(conversation, chats: store.bots)
        Button {
            guard let accounts, let chat = conversation.chatBotId, opens else { return }
            dismiss()
            // The chat shows the conversation's notice with its chip; this goes straight to the thread.
            store.pendingThread = conversation.threadRoot.map { PendingThread(chat: chat, root: $0) }
            accounts.selection = BotReference(accountId: accounts.accountId, computerId: store.computer.id, botId: chat)
        } label: {
            HStack(spacing: 12) {
                agentAvatar(conversation.agent)
                Text(conversation.originLabel ?? conversation.agent)
                    .foregroundStyle(Palette.text).lineLimit(1)
                Spacer(minLength: 8)
                if let last = conversation.last {
                    Text(RelativeTime.day(last)).font(.subheadline).foregroundStyle(Palette.secondary)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!opens)
    }
}
