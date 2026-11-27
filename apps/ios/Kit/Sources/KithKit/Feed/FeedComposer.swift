import Foundation
import Observation

/// The Feed's bottom box. An @ chip picks the agent; with none, the computer picks one for the text (`feedRoute`)
/// and the box shows it as a chip you can change. Nothing is sent on a pick for words the person has not seen it
/// chosen for: a pick belongs to the exact text it was made for.
@MainActor
@Observable
public final class FeedComposer {
    public struct Agent: Identifiable, Hashable, Sendable {
        /// The kith agent id.
        public let id: String
        public let name: String
    }

    public struct Pick: Equatable, Sendable {
        public var agent: String
        public var why: String
        public var others: [String]
        /// The trimmed text it was made for.
        public var forText: String
        /// The computer could not route (an older host): the home agent stands in.
        public var fallback: Bool
    }

    public struct Sent: Equatable, Sendable {
        public var agent: String
        public var chat: String
    }

    public private(set) var text = ""
    /// The agent the person tapped; it wins over any pick.
    public private(set) var tag: String?
    public private(set) var pick: Pick?
    public private(set) var routing = false
    public private(set) var sending = false
    public private(set) var sent: Sent?
    public var error: String?

    @ObservationIgnored private let host: @MainActor () -> (any FeedHost)?
    @ObservationIgnored private let chats: @MainActor () -> [Bot]
    @ObservationIgnored private let startChat: @MainActor (String) async throws -> Bot
    @ObservationIgnored private let home: @MainActor () -> String
    @ObservationIgnored private let debounce: Duration
    @ObservationIgnored private var routeTask: Task<Void, Never>?

    public init(host: @escaping @MainActor () -> (any FeedHost)?, chats: @escaping @MainActor () -> [Bot],
                startChat: @escaping @MainActor (String) async throws -> Bot,
                home: @escaping @MainActor () -> String = { "main" }, debounce: Duration = .milliseconds(600)) {
        self.host = host
        self.chats = chats
        self.startChat = startChat
        self.home = home
        self.debounce = debounce
    }

    public var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// The agents an @ chip can name: the owner's kith agents, one each.
    public var agents: [Agent] {
        var seen = Set<String>()
        return chats().compactMap { bot in
            guard !bot.isGroup, let id = bot.hqAgent, seen.insert(id).inserted else { return nil }
            return Agent(id: id, name: bot.name)
        }
    }

    public func name(of agent: String) -> String { agents.first { $0.id == agent }?.name ?? agent }

    /// The agent this would go to, or nil while no choice has been shown for the current words.
    public var target: String? {
        if let tag { return tag }
        guard let pick, pick.forText == trimmed else { return nil }
        return pick.agent
    }

    public var canSend: Bool { !trimmed.isEmpty && target != nil && !sending }

    public func edit(_ new: String) {
        text = new
        sent = nil
        error = nil
        guard tag == nil else { return }
        routeTask?.cancel()
        guard !trimmed.isEmpty else { pick = nil; routing = false; return }
        routing = true
        let words = trimmed
        routeTask = Task { [weak self, debounce] in
            try? await Task.sleep(for: debounce)
            guard !Task.isCancelled else { return }
            await self?.route(words)
        }
    }

    /// Taps an @ chip; tapping the same one again lets the computer choose.
    public func choose(_ agent: String) {
        if tag == agent {
            tag = nil
            edit(text)
        } else {
            routeTask?.cancel()
            routing = false
            tag = agent
        }
    }

    private func route(_ words: String) async {
        defer { if words == trimmed { routing = false } }
        guard let host = host() else { return }
        let result: Pick
        do {
            let r = try await host.feedRoute(text: words)
            result = Pick(agent: r.agent, why: r.why, others: r.others, forText: words, fallback: false)
        } catch {
            // Until the route exists the computer says "not yet": the home agent is shown and can be changed.
            result = Pick(agent: home(), why: "", others: [], forText: words, fallback: true)
        }
        guard !Task.isCancelled, words == trimmed else { return }
        pick = result
    }

    /// Opens (or reuses) the chat with the agent and sends; the box clears only when it went.
    public func send() async {
        guard canSend, let agent = target, let host = host() else { return }
        let words = trimmed
        sending = true
        error = nil
        defer { sending = false }
        do {
            let existing = chats().first { !$0.isGroup && $0.hqAgent == agent }
            let chat: String
            if let existing { chat = existing.id } else { chat = try await startChat(agent).id }
            try await host.sendText(botId: chat, threadId: nil, text: words, clientNonce: UUID().uuidString)
            routeTask?.cancel()
            text = ""
            tag = nil
            pick = nil
            routing = false
            sent = Sent(agent: agent, chat: chat)
        } catch {
            self.error = error.localizedDescription
        }
    }
}
