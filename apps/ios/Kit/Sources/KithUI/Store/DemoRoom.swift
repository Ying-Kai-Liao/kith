import KithKit
import Foundation

#if DEBUG

/// The `room-*` demos: the agent-conversation pieces in a group room ("Launch page" with Ada and Remy, and Grok).
/// Names: `room-turns` (the turns popover over the box), `room-working` (the live line), `room-stopped` (a stop line and a
/// late reply), `room-spent` (out of turns), `room-add` (the Add sheet with Grok picked), `room-grok` (Grok in the room,
/// the room waiting on it), `room-info`.
extension DemoScreens {
    public static let roomId = "launch"
    public static var isRoom: Bool { name?.hasPrefix("room-") == true }
    /// The demos that need Grok to be a member.
    static var roomHasGrok: Bool { name == "room-grok" || name == "room-info" }
}

extension AccountStore {
    @MainActor
    static func seedRoom(_ store: BotStore, now: Int64) {
        guard let screen = DemoScreens.name, DemoScreens.isRoom else { return }
        let id = DemoScreens.roomId, minute: Int64 = 60_000
        func decode<T: Decodable>(_ json: [String: Any]) -> T? {
            (try? JSONSerialization.data(withJSONObject: json)).flatMap { try? JSONDecoder().decode(T.self, from: $0) }
        }
        var members = ["ada", "remy"]
        if DemoScreens.roomHasGrok { members.append("bg_grok") }
        if let room: Bot = decode([
            "id": id, "name": "Launch page", "kind": "group", "rev": 1, "status": "idle", "unread": 0, "pinned": false,
            "members": members, "lastAt": now - 2 * minute, "avatarSource": "auto",
        ]) { store.bots[id] = room }

        store.botGuests.seedDemo([
            BotGuest(botId: "bg_grok", name: "Grok", provider: "grok", state: .active, agents: [], limits: BotGuestLimits(ratePerHour: 60, concurrent: 2, turnTimeoutS: 300, wakeProfile: "grok/kai"),
                     connected: true, createdAt: now - 80 * 60 * minute, personId: "bot-1"),
            BotGuest(botId: "bg_muse", name: "Muse", provider: "other", state: .pending, agents: [], createdAt: now - 60 * minute),
            BotGuest(botId: "bg_codex", name: "Sam's Codex", provider: "other", state: .active, ownerPerson: "sam", ownerName: "Sam", agents: [],
                     createdAt: now - 90 * 60 * minute, personId: "bot-2"),
        ])

        var seq: Int64 = 0
        func entry(_ kind: String, _ eid: String? = nil, ago: Int64, data: [String: Any]) -> Entry? {
            seq += 1
            return decode(["id": eid ?? "r\(seq)", "seq": seq, "botId": id, "rev": seq, "kind": kind, "turn": seq, "data": data,
                           "createdAt": now - ago * minute, "updatedAt": now - ago * minute])
        }
        func person(_ eid: String, _ text: String, ago: Int64) -> Entry? { entry("user", eid, ago: ago, data: ["text": text]) }
        func agent(_ by: String, _ text: String, ago: Int64, conversation: String? = "cv1", late: String? = nil) -> Entry? {
            var data: [String: Any] = ["text": text, "final": true, "author": by]
            if let conversation { data["conversation"] = conversation }
            if let late { data["late"] = true; data["lateWhy"] = late }
            return entry("agent", ago: ago, data: data)
        }
        func grok(_ text: String, ago: Int64, conversation: String = "cv1") -> Entry? {
            entry("user", ago: ago, data: ["text": text, "conversation": conversation,
                                           "sender": ["botId": "bg_grok", "name": "Grok", "provider": "grok"]])
        }
        let opening = "Plan the launch page hero. @Ada the layout, @Remy what it costs to build."
        func conversation(_ state: String, why: String? = nil, left: Int, current: [String: Any]? = nil, waitUntil: Int64? = nil) -> RoomConversation? {
            var json: [String: Any] = ["conversation": "cv1", "chat": id, "budget": 10, "left": left, "state": state, "opening": "o1"]
            if let why { json["endedWhy"] = why }
            if let current { json["current"] = current }
            if let waitUntil { json["waitUntil"] = waitUntil }
            return decode(json)
        }

        var list: [Entry?] = []
        switch screen {
        case "room-turns":
            list = [
                person("o0", "Is the beta date still Friday?", ago: 1500),
                agent("remy", "Yes. The build is on TestFlight, the page is the last piece.", ago: 1499, conversation: nil),
                agent("ada", "I'll have the hero layout by tonight.", ago: 1498, conversation: nil),
            ]
            store.setComposerDraft("Plan the launch page hero", for: id)
        case "room-working":
            list = [
                person("o1", opening, ago: 20),
                agent("ada", "Sky at the top, the crew walking in from the right, one line: \"Your crew, on your phone and Mac.\"", ago: 19),
                agent("remy", "The walk-in is a Lottie we already have. Two hours with the sky crop. @Ada do you want it on mobile too?", ago: 18),
                agent("ada", "Yes, but still: the first frame only, no motion under 600 px.", ago: 17),
            ]
            if let c = conversation("open", left: 6, current: ["id": "remy", "kind": "agent", "name": "Remy"]) { store.roomConversations.note(c) }
        case "room-stopped":
            list = [
                person("o1", opening, ago: 20),
                agent("ada", "Sky at the top, the crew walking in from the right, one line: \"Your crew, on your phone and Mac.\"", ago: 19),
                agent("remy", "The walk-in is a Lottie we already have. Two hours with the sky crop. @Ada do you want it on mobile too?", ago: 18),
                agent("ada", "Yes, but still: the first frame only, no motion under 600 px.", ago: 17),
                agent("remy", "Then mobile is a static PNG, 20 minutes more. I'll draft the copy block next.", ago: 15, late: "stopped"),
            ]
            if let c = conversation("stopped", why: "stopped", left: 6) { store.roomConversations.note(c) }
        case "room-spent":
            list = [
                person("o1", "Keep going on the copy, both of you.", ago: 20),
                agent("ada", "Headline stays. Sub: \"Agents that remember, on the Mac you already have.\"", ago: 19),
                agent("remy", "Fits in two lines at 390 pt. Shipping it behind the beta flag?", ago: 18),
                agent("ada", "Behind the flag. @Remy the OG image too.", ago: 17),
            ]
            if let c = conversation("ended", why: "spent", left: 0) { store.roomConversations.note(c) }
        case "room-grok":
            list = [
                entry("notice", "n1", ago: 25, data: ["text": "Grok joined · sees this room from now on", "notice": "botJoined", "style": "info"]),
                person("o1", "@Grok what do people on X say about \"Your crew\" as a name?", ago: 24),
                grok("Mostly warm. \"Crew\" reads as people you trust; two replies called it a gaming word. Nobody confused it with a product.", ago: 23),
                agent("ada", "Good. Keeping it. @Grok any name clash in the AI space?", ago: 22),
            ]
            if let c = conversation("open", left: 7, current: ["id": "bg_grok", "kind": "bot", "name": "Grok"], waitUntil: now + 5 * minute) {
                store.roomConversations.note(c)
            }
        default:
            // room-add and room-info sit over the room as it is.
            list = [
                entry("notice", "n1", ago: 25, data: ["text": "Grok joined · sees this room from now on", "notice": "botJoined", "style": "info"]),
                person("o1", "@Grok what do people on X say about \"Your crew\" as a name?", ago: 24),
            ]
            if !DemoScreens.roomHasGrok { list = [person("o1", opening, ago: 20)] }
        }
        if screen != "room-turns" { store.setComposerDraft("", for: id) }
        store.entries[id] = list.compactMap { $0 }
        store.historyComplete.insert(id)
    }
}

#endif
