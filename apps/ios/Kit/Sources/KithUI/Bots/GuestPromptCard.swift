import KithKit
import SwiftUI

/// A guest's permission request waiting for the owner: who is asking, what for, and only the answers the card offers.
/// The data and the rules are `GuestPrompt`'s; this lays them out, so a restyled decision card can replace it.
public struct GuestPromptCard: View {
    let prompt: GuestPrompt
    /// The option whose answer is on its way to the computer, if any.
    let answering: String?
    let respond: (String) -> Void

    public init(prompt: GuestPrompt, answering: String?, respond: @escaping (String) -> Void) {
        self.prompt = prompt
        self.answering = answering
        self.respond = respond
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(prompt.who).font(.headline).foregroundStyle(Palette.text)
            if !prompt.request.isEmpty {
                Text(prompt.request)
                    .font(.subheadline.monospaced())
                    .foregroundStyle(Palette.secondary)
                    .lineLimit(4)
            }
            VStack(spacing: 0) {
                ForEach(Array(prompt.options.enumerated()), id: \.element.id) { i, option in
                    if i > 0 { Rectangle().fill(Palette.border).frame(height: 0.5) }
                    Button { respond(option.optionId) } label: {
                        HStack(spacing: 8) {
                            Text(GuestPrompt.label(option, edge: prompt.entry.data.edge))
                                .font(.body.weight(option.kind == "allow_once" ? .semibold : .regular))
                                .foregroundStyle(option.kind.hasPrefix("allow") ? Palette.text : Palette.danger)
                            Spacer(minLength: 0)
                            if answering == option.optionId { Spinner(size: 14) }
                        }
                        .frame(maxWidth: .infinity, minHeight: 46, alignment: .leading)
                        .padding(.horizontal, 14)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(answering != nil)
                    .opacity(answering == nil || answering == option.optionId ? 1 : 0.4)
                }
            }
            .background(Palette.background, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .padding(16)
        .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        .accessibilityElement(children: .contain)
    }
}

/// A key or account card waiting outside any chat (an agent's card lane): the same card a chat shows.
public struct SecretPromptCard: View {
    let prompt: GuestPrompt
    let store: BotStore

    public init(prompt: GuestPrompt, store: BotStore) {
        self.prompt = prompt
        self.store = store
    }

    public var body: some View {
        let entry = prompt.entry
        let bot = entry.data.agent.flatMap(store.agentBot)
        let agent = bot?.name ?? entry.data.agent ?? "The agent"
        let tint = AgentTint.resolve(bot?.avatarColor)
        if entry.data.card == "key" {
            KeyCard(entry: entry, agentName: agent, hostName: store.hostName, tint: tint, answering: store.answering[entry.id],
                    save: { account, values in try await store.provideSecret(entry, account: account, values: values) },
                    deny: { store.respond(entry, option: "deny") })
        } else {
            AccessCard(entry: entry, agentName: agent, tint: tint, answering: store.answering[entry.id]) { option in
                store.respond(entry, option: option)
            }
        }
    }
}

/// What opening a guest prompt from its push shows: the card for that chat, a moment of loading while the computer's
/// events arrive, then "already answered" when none is waiting.
public struct GuestPromptSheet: View {
    let store: BotStore
    let chatId: String
    /// The account's role on this computer (see `GuestPrompt.pending`).
    let isOwner: Bool
    let close: () -> Void
    /// The chat turned out to be one of the owner's own bots once the roster arrived.
    let openChat: () -> Void
    @State private var waited = false

    public init(store: BotStore, chatId: String, isOwner: Bool, close: @escaping () -> Void, openChat: @escaping () -> Void) {
        self.store = store
        self.chatId = chatId
        self.isOwner = isOwner
        self.close = close
        self.openChat = openChat
    }

    private var prompts: [GuestPrompt] { store.guestPrompts(isOwner: isOwner).filter { $0.chatId == chatId } }

    public var body: some View {
        VStack(spacing: 14) {
            Text("Needs your OK").font(.headline).foregroundStyle(Palette.text)
            if prompts.isEmpty {
                if waited {
                    Text("Already answered.").font(.subheadline).foregroundStyle(Palette.secondary)
                    Button("Done", action: close).buttonStyle(.secondary)
                } else {
                    Spinner(size: 18)
                }
            } else {
                ForEach(prompts) { prompt in
                    if prompt.entry.data.isSecretCard {
                        SecretPromptCard(prompt: prompt, store: store)
                    } else {
                        GuestPromptCard(prompt: prompt, answering: store.answering[prompt.entry.id]) {
                            store.respond(prompt.entry, option: $0)
                        }
                    }
                }
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity)
        .background(Palette.background)
        .task {
            // The event stream may still be catching up when a push opens this.
            try? await Task.sleep(for: .seconds(6))
            waited = true
        }
        .onChange(of: store.bots[chatId] != nil) { _, known in if known { openChat() } }
    }
}
