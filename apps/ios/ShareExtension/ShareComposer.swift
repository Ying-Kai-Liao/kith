import KithKit
import SwiftUI

/// The share sheet's one screen: what is being shared, a note, and the chat it goes to.
struct ShareComposer: View {
    let content: ShareContent
    let context: SharedStore.Context
    let finish: () -> Void
    let cancel: () -> Void

    @State private var note = ""
    @State private var selected: BotReference?

    private var chats: [BotSnapshot] {
        context.bots.filter { !$0.bot.hidden }.sorted { $0.bot.lastAt > $1.bot.lastAt }
    }

    private func reference(_ chat: BotSnapshot) -> BotReference {
        BotReference(accountId: context.accountID, computerId: chat.computerId, botId: chat.bot.id)
    }

    var body: some View {
        NavigationStack {
            Group {
                if content.isEmpty {
                    ContentUnavailableView("Nothing to send", systemImage: "link", description: Text("Kith takes links and text."))
                } else if chats.isEmpty {
                    ContentUnavailableView("No chats yet", systemImage: "bubble.left",
                                           description: Text("Open Kith and connect a computer first."))
                } else {
                    form
                }
            }
            .navigationTitle("Send to Kith")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: cancel) }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Send", action: send).disabled(selected == nil || content.isEmpty)
                }
            }
        }
        .onAppear {
            guard selected == nil else { return }
            let last = context.lastShareTarget
            selected = chats.map(reference).first { $0 == last } ?? chats.first.map(reference)
        }
    }

    private var form: some View {
        List {
            Section {
                VStack(alignment: .leading, spacing: 4) {
                    if let title = content.title, !title.isEmpty { Text(title).font(.headline).lineLimit(2) }
                    Text(content.link?.absoluteString ?? content.text ?? "")
                        .font(.subheadline).foregroundStyle(.secondary).lineLimit(3)
                }
                TextField("Add a note", text: $note, axis: .vertical).lineLimit(1...4)
            }
            Section("Send to") {
                ForEach(chats, id: \.self) { chat in
                    Button {
                        selected = reference(chat)
                    } label: {
                        HStack {
                            Text(chat.bot.name).foregroundStyle(.primary)
                            Spacer()
                            if selected == reference(chat) { Image(systemName: "checkmark") }
                        }
                    }
                }
            }
        }
    }

    private func send() {
        guard let selected else { return }
        let text = SharedLink.message(link: content.link, title: content.title, text: content.text, note: note)
        context.queueShare(PendingShare(target: selected, text: text))
        finish()
    }
}
