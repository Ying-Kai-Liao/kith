import Foundation
import Testing
@testable import KithKit
@testable import KithUI

@MainActor
struct LeftThreadRoutingTests {
    private func store(suite: String) -> BotStore {
        let key = Data(repeating: 3, count: 32)
        let encoded = key.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        let computer = Computer(id: RelayCrypto.computerId(signKey: key), name: "Mac", signKey: encoded)
        return BotStore(computer: computer, storage: SharedStore.Context(accountID: "t", suite: suite)) { FakeRemote(.connecting) }
    }

    private func entry(_ id: String, thread: String? = nil, task: Bool = false) -> Entry {
        var data = EntryData()
        data.task = task ? true : nil
        return Entry(id: id, seq: 0, botId: "a", threadId: thread, rev: 1, kind: "user", turn: 1, data: data, createdAt: 0, updatedAt: 0)
    }

    /// A Reply on a task's row, or on a message in its thread, points the chat's box at that task; a message outside
    /// any task changes nothing.
    @Test func replyingPointsTheBoxAtTheMessagesTask() {
        let suite = "TaskTargets.\(UUID().uuidString)"
        defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
        let model = store(suite: suite)
        model.target(taskOf: entry("plain"))
        #expect(model.taskTargets["a"] == nil)
        model.target(taskOf: entry("root", task: true))
        #expect(model.taskTargets["a"] == .task("root"))
        model.target(taskOf: entry("r1", thread: "other"))
        #expect(model.taskTargets["a"] == .task("other"))
    }

    @Test func aChatWithoutTasksHasNoDestination() {
        let suite = "TaskTargets.\(UUID().uuidString)"
        defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
        #expect(store(suite: suite).destination("a") == nil)
    }
}
