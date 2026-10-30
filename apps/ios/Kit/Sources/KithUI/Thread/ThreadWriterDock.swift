import KithKit
import SwiftUI

/// Takes the chat box's place while a terminal (or another writer) holds a thread: the banner says who has it, and
/// Take over asks the host to close the terminal session. A refusal (the terminal is busy, or it didn't close)
/// shows as "Couldn't take over" with Try again.
struct ThreadWriterDock: View {
    let botId: String
    /// The thread's root message.
    let rootId: String
    @Environment(BotStore.self) private var model
    @State private var taking = false
    @State private var failed = false
    @State private var confirming = false

    private var bot: Bot? { model.bots[botId] }
    private var root: Entry? { model.allEntries(botId).first { $0.id == rootId } }

    var body: some View {
        if let phase = ThreadFeed.writerPhase(root?.data.thread, taking: taking, failed: failed, unreachable: model.isOffline) {
            WriterBanner(phase: phase, mac: model.hostName, tint: AgentTint.resolve(bot?.avatarColor), pending: taking) {
                confirming = true
            }
            .padding(.horizontal, KithSpacing.gutter)
            .padding(.top, 6)
            .padding(.bottom, 8)
            .kithSheet(isPresented: $confirming) {
                TakeOverSheet(mac: model.hostName, command: ThreadFeed.resumeCommand(root: root, bot: bot), pending: taking,
                              onConfirm: takeOver, onCancel: { confirming = false })
            }
        }
    }

    /// One take-over at a time: a second tap while it runs does nothing.
    private func takeOver() {
        guard !taking else { return }
        taking = true
        failed = false
        Task {
            do {
                _ = try await model.ready().takeOverThread(botId: botId, threadId: rootId)
            } catch {
                failed = true
                model.lastError = (error as? ThreadWriterError)?.errorDescription ?? error.localizedDescription
            }
            taking = false
            confirming = false
        }
    }
}

/// What fills the space under a chat.
enum ChatBottom: Equatable {
    case updateNeeded
    /// The banner only: a bot guest's chat is read-only even while a terminal holds its thread.
    case writerBanner(rootId: String)
    /// The banner, and under it the composer: a message to the held thread waits for the terminal (the host queues it,
    /// the thread stays single-writer), and New task starts another.
    case writerBannerAndComposer(rootId: String)
    case readOnly
    case composer
}

extension ThreadFeed {
    static func chatBottom(held: Entry?, mismatch: Bool, guest: Bool) -> ChatBottom {
        if mismatch { return .updateNeeded }
        if let held { return guest ? .writerBanner(rootId: held.id) : .writerBannerAndComposer(rootId: held.id) }
        return guest ? .readOnly : .composer
    }
}

extension ThreadFeed {
    /// The command that continues a thread in a terminal; empty when the root or the bot is gone.
    static func resumeCommand(root: Entry?, bot: Bot?) -> String {
        guard let root, let bot else { return "" }
        return root.resumeCommand(agent: bot.name)
    }

    /// The root of the thread the chat's box goes on with (the newest task) when somebody else holds it.
    static func heldTask(in chat: [Entry]) -> Entry? {
        Tasks.current(in: chat).flatMap { $0.data.thread?.isHeldElsewhere == true ? $0 : nil }
    }
}
