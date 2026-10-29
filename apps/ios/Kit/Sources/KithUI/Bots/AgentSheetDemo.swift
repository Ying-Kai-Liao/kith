#if DEBUG
import KithKit
import SwiftUI

/// The `-kithDemoScreens agent`, `agent-settings` and `agent-memory` shots: the agent sheet over a fixed hq bot
/// and a fixed kith answer. Nothing connects; `KithInfoStore.load` takes the seeded answer.
public enum DemoAgent {
    public static let botId = "coder-hq"

    /// The sheet's tab for a demo screen name.
    static func tab(_ screen: String) -> DetailsTab {
        switch screen {
        case "agent-memory": .memory
        case "agent-settings": .settings
        case "agent-access": .access
        default: .details
        }
    }

    static func response(for botId: String) -> KithInfoResponse? {
        guard DemoScreens.active, botId == Self.botId else { return nil }
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let json: [String: Any] = [
            "info": [
                "agent": "coder", "description": "Writes and fixes code in your repos.", "harness": "Claude Code 2.1.4",
                "model": "opus", "permission_mode": "default",
                "role": ["intro": "Writes and fixes code in your repos. Opens a PR for every change and never pushes to main."],
                "package": ["version": "1.4", "dirty": false, "unpushed": true],
                "skills": [["name": "thread-writers", "description": "Model-checks one writer per thread", "source": "package"],
                           ["name": "flow", "description": "Runs workers in worktrees", "source": "package"]],
                "tools": ["Bash", "Edit", "Read", "gh"],
                "plugins": ["github", "context7"],
                "projects": [["id": "kith", "files": 41, "modified": now - 3_600_000],
                             ["id": "peace-guardian", "files": 12, "modified": now - 86_400_000]],
                "schedules": [["id": "tidy", "cron": "0 3 * * *", "task": "Nightly tidy", "paused": false,
                               "last_status": "ok", "last_run": now - 8 * 3_600_000, "next_run": now + 16 * 3_600_000]],
                "cost": ["today": 0.62, "week": 2.4, "month": 4.12, "all": 31.9, "requests_month": 212,
                         "by_model_month": [["model": "opus", "usd": 3.8], ["model": "sonnet", "usd": 0.32]]],
                "errors": [String: String](),
            ] as [String: Any],
            "fetchedAt": now,
        ]
        return (try? JSONSerialization.data(withJSONObject: json)).flatMap { try? JSONDecoder().decode(KithInfoResponse.self, from: $0) }
    }
}

/// Presents the agent sheet's content as the whole screen, for the demo shots.
public struct DemoAgentSheet: View {
    let store: BotStore
    let tab: DetailsTab

    public init(store: BotStore, screen: String) {
        self.store = store
        tab = DemoAgent.tab(screen)
        let json: [String: Any] = [
            "id": DemoAgent.botId, "name": "coder", "kind": "agent", "rev": 1, "status": "idle", "unread": 0, "members": [String](),
            "pinned": false, "lastAt": Int64(Date().timeIntervalSince1970 * 1000), "avatarColor": "kith:cyan", "backend": "claude",
            "model": "opus", "hqAgent": "coder", "notify": true,
        ]
        if let bot = (try? JSONSerialization.data(withJSONObject: json)).flatMap({ try? JSONDecoder().decode(Bot.self, from: $0) }) {
            store.bots[bot.id] = bot
        }
    }

    public var body: some View {
        BotDetailsView(botId: DemoAgent.botId, request: DetailsRequest(tab: tab), call: CallAction(blocked: nil) {})
            .environment(store)
            .tint(Palette.accent)
    }
}
#endif
