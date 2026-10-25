import KithKit
import SwiftUI

/// A thread (Slack's "reply in thread"): the message it started on, its replies,
/// and a box to continue there. In a bot's own chat the thread is a separate branch
/// of the conversation (its own session, forked from the chat); in a group the room
/// answers inside the thread.
struct RepliesView: View {
    let botId: String
    let rootId: String
    let close: () -> Void
    @Environment(BotStore.self) var model
    @State var showTrace = false
    @State var following = true

    var chat: Bot? { model.bots[botId] }
    var root: Entry? { model.allEntries(botId).first { $0.id == rootId } }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 0) {
                    Text("Thread").kithText(.topBar).foregroundStyle(Palette.ink)
                    Text(chat?.name ?? "").kithText(.secondary).foregroundStyle(Palette.muted).lineLimit(1)
                    PlaceFolderChip(botId: botId, thread: rootId)
                }
                Spacer(minLength: 8)
                if let root, let chat {
                    Menu {
                        Button("Copy resume command", systemImage: "terminal") { Pasteboard.copy(root.resumeCommand(agent: chat.name)) }
                    } label: {
                        Image(systemName: "ellipsis").font(.system(size: 17, weight: .medium)).foregroundStyle(Palette.ink)
                            .frame(width: 44, height: 44).contentShape(Rectangle())
                    }
                    .accessibilityLabel("Thread actions")
                }
                IconButton("Close thread", systemImage: "xmark", action: close)
                    .keyboardShortcut(.cancelAction)
            }
            .padding(.leading, 20)
            .padding(.trailing, 12)
            .frame(height: 56)
            Rectangle().fill(Palette.border).frame(height: 0.5)

            // The replies: each platform's own `messages`, in its `RepliesView+` file.
            messages
                .safeAreaInset(edge: .bottom) {
                    if let mismatch = model.mismatch {
                        UpdateNeededCard(store: model, mismatch: mismatch)
                            .padding(.horizontal, 16)
                            .padding(.bottom, 8)
                    } else if root?.data.thread?.isHeldElsewhere == true {
                        ThreadWriterDock(botId: botId, rootId: rootId)
                    } else if let guest = chat?.botGuest {
                        ReadOnlyLine(text: guest.readOnlyLine)
                    } else {
                        Composer(botId: botId, thread: rootId)
                    }
                }
        }
        .background(Palette.background)
        .task(id: rootId) { await model.loadThread(botId, root: rootId) }
        // Back in the main chat, the next message is not a reply here.
        .onDisappear { if chat?.usesTasks == true { model.leftThread.insert(botId) } }
        // A thread is read on its own: having it open reads its replies.
        .readingConversation(botId, thread: rootId)
        .kithSheet(item: Binding<FileDownloads.Export?>(get: {
            guard let item = model.fileDownloads.export, item.botId == botId, item.threadId == rootId else { return nil }
            return item
        }, set: { if $0 == nil { model.fileDownloads.dismissExport() } })) { item in
            FileExportSheet(url: item.url) { model.fileDownloads.dismissExport() }
        }
        .kithSheet(isPresented: $showTrace) {
            TraceView(botId: botId, thread: rootId)
        }
    }
}

/// Which thread is open.
struct ThreadTarget: Identifiable, Equatable {
    let id: String
}

extension RepliesView {
    /// iPhone: the replies follow the newest one until the reader scrolls away.
    var messages: some View {
        let replies = model.replies(botId, root: rootId)
        let working = chat?.isWorking(in: botId, thread: rootId) == true && !model.isOffline
        var rows: [ConversationRow] = []
        if let root {
            // The host's count, as on the thread's chip: messages, not the agent's trace.
            let count = root.data.thread?.count ?? 0
            rows.append(ConversationRow("root") {
                ChatRow(entry: root, groupStart: true, chat: chat) { showTrace = true }
                    .equatable()
            })
            rows.append(ConversationRow("count") {
                HStack(spacing: 10) {
                    Text(count == 0 ? "No replies yet" : count == 1 ? "1 reply" : "\(count) replies")
                        .kithText(.secondary)
                        .foregroundStyle(Palette.muted)
                        .fixedSize()
                    Rectangle().fill(Palette.border).frame(height: 1)
                }
                .padding(.vertical, 12)
            })
        }
        for reply in ThreadFeed.replies(replies, separators: true) {
            switch reply {
            case .entry(let e, let groupStart):
                rows.append(ConversationRow(reply.id, isUserMessage: e.kind == "user") {
                    ChatRow(entry: e, groupStart: groupStart, chat: chat) { showTrace = true }
                        .equatable()
                })
            case .separator(let date, _):
                rows.append(ConversationRow(reply.id) { TimeSeparator(date: date) })
            case .tool(let e):
                rows.append(ConversationRow(reply.id) { ToolCallRow(ThreadFeed.toolRow(e)).padding(.top, 4) })
            case .earlier(let count, _):
                rows.append(ConversationRow(reply.id) {
                    Button { showTrace = true } label: {
                        Text(count == 1 ? "1 earlier step" : "\(count) earlier steps")
                            .kithText(.secondary).foregroundStyle(Palette.muted).frame(minHeight: 32, alignment: .leading)
                    }
                    .buttonStyle(.plain)
                })
            }
        }
        if let chat, working {
            rows.append(ConversationRow("working") {
                WorkingIndicator(bot: chat, thinking: model.currentThinking(botId, thread: rootId))
                    .padding(.top, 6)
            })
        }
        return ConversationList(rows: rows, following: $following)
    }
}
