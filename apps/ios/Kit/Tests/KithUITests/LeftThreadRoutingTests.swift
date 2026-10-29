import Foundation
import Testing
@testable import KithKit
@testable import KithUI

@MainActor
struct LeftThreadRoutingTests {
    @Test func leavingAThreadStartsANewTaskInThatBotsMainChatOnly() {
        let suite = "LeftThreadRouting.\(UUID().uuidString)"
        defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
        let key = Data(repeating: 3, count: 32)
        let encoded = key.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        let computer = Computer(id: RelayCrypto.computerId(signKey: key), name: "Mac", signKey: encoded)
        let model = BotStore(computer: computer, storage: SharedStore.Context(accountID: "t", suite: suite)) { FakeRemote(.connecting) }
        #expect(!model.startsNewTask("a", thread: nil, requested: false))
        model.leftThread.insert("a")
        #expect(model.startsNewTask("a", thread: nil, requested: false))
        // Inside an open thread a send still goes to it; another bot is unaffected.
        #expect(!model.startsNewTask("a", thread: "t1", requested: false))
        #expect(!model.startsNewTask("b", thread: nil, requested: false))
        // An explicit "New task" always wins; sending clears the flag.
        #expect(model.startsNewTask("b", thread: nil, requested: true))
        model.leftThread.remove("a")
        #expect(!model.startsNewTask("a", thread: nil, requested: false))
    }

    /// The main chat's send continues the current task only while it shows open (the idle rule, Tasks.isOpen).
    @Test func aFoldedCurrentTaskStartsANewOneInTheMainChat() {
        let suite = "FoldedRouting.\(UUID().uuidString)"
        defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
        let key = Data(repeating: 4, count: 32)
        let encoded = key.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        let computer = Computer(id: RelayCrypto.computerId(signKey: key), name: "Mac", signKey: encoded)
        let model = BotStore(computer: computer, storage: SharedStore.Context(accountID: "t", suite: suite)) { FakeRemote(.connecting) }
        let now: Int64 = 10_000_000
        // Open task (done, idle under ten minutes, no choice): continues.
        let open = Tasks.isOpen(.done, newest: true, lastAt: now - 60_000, now: now)
        #expect(!model.startsNewTask("a", thread: nil, requested: false, currentOpen: open))
        // Folded by hand: new task.
        let folded = Tasks.isOpen(.done, newest: true, lastAt: now - 60_000, now: now, choice: false)
        #expect(model.startsNewTask("a", thread: nil, requested: false, currentOpen: folded))
        // Folded by itself once idle: new task.
        let idle = Tasks.isOpen(.done, newest: true, lastAt: now - Tasks.idleMs, now: now)
        #expect(model.startsNewTask("a", thread: nil, requested: false, currentOpen: idle))
        // A waiting question opens the task even when folded (BotStore.isCurrentTaskOpen ORs it in): continues.
        #expect(!model.startsNewTask("a", thread: nil, requested: false, currentOpen: true))
        // No task yet: nothing to fold. Inside a thread: that thread, folded or not.
        #expect(!model.startsNewTask("a", thread: nil, requested: false, currentOpen: nil))
        #expect(!model.startsNewTask("a", thread: "t1", requested: false, currentOpen: false))
        // Left the thread: new even though the task is open; another bot is unaffected.
        model.leftThread.insert("a")
        #expect(model.startsNewTask("a", thread: nil, requested: false, currentOpen: true))
        #expect(!model.startsNewTask("b", thread: nil, requested: false, currentOpen: true))
    }
}
