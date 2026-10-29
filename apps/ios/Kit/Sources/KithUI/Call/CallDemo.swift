#if DEBUG
import KithKit
import Observation
import SwiftUI

/// A fixed engine for the `-kithDemoScreens call` shots: a phase, captions and a level, nothing connects.
@MainActor
@Observable
final class DemoVoiceEngine: VoiceEngine {
    var phase: VoicePhase = .speaking
    var level: Float = 0.55
    var muted = false
    var provider: VoiceProvider? { nil }
    var onActivityChanged: ((Bool) -> Void)?
    var captions: [CallCaption] = [
        CallCaption(id: 0, speaker: .you, text: "Can you check why the build failed?"),
        CallCaption(id: 1, speaker: .bot, text: "The lint step failed on two files. I fixed both and pushed a commit."),
        CallCaption(id: 2, speaker: .you, text: "Great, run the tests again."),
        CallCaption(id: 3, speaker: .bot, text: "Running them now. All 214 passed."),
    ]
    var supportsSpeaker: Bool { true }

    func start() async {}
    func end() {}
    func interrupt() {}
    func speak(reply: String) {}
    func announce(_ notice: String) {}
}

/// The call screens over a seeded bot, as the whole screen, for the demo shots.
public struct DemoCallScreen: View {
    let store: BotStore
    let incoming: Bool
    @State private var engine = DemoVoiceEngine()

    static let botId = "coder-call"

    public init(store: BotStore, incoming: Bool) {
        self.store = store
        self.incoming = incoming
        let json: [String: Any] = [
            "id": Self.botId, "name": "coder", "kind": "agent", "rev": 1, "status": "idle", "unread": 0, "members": [String](),
            "pinned": false, "lastAt": Int64(Date().timeIntervalSince1970 * 1000), "avatarColor": "kith:violet", "backend": "claude",
            "model": "opus", "hqAgent": "coder", "notify": true,
        ]
        if let bot = (try? JSONSerialization.data(withJSONObject: json)).flatMap({ try? JSONDecoder().decode(Bot.self, from: $0) }) {
            store.bots[bot.id] = bot
        }
    }

    public var body: some View {
        if incoming {
            IncomingCallView(
                call: IncomingCall(id: "demo", botId: Self.botId, name: "coder",
                                   reason: "The release build is green. Want to talk through what to ship?",
                                   deadline: Date().addingTimeInterval(45)),
                bot: store.bots[Self.botId], accept: {}, decline: {}, reply: {}, snooze: {})
        } else {
            CallScreen(botId: Self.botId, close: {}, engine: engine).environment(store)
        }
    }
}
#endif
