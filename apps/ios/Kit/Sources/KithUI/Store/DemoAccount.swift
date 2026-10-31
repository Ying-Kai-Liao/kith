import KithKit
import Foundation

/// DEBUG-only screenshot hook: launch with `-kithDemoScreens <name>` and the app shows real screens over a
/// fixed, host-less account (`AccountStore.demo`). CI (`.github/workflows/screens.yml`) drives it.
/// A new screen adds one case to the switch in `KithApp.showDemoScreen`. Release builds see `active == false`.
public enum DemoScreens {
    #if DEBUG
    /// The screen asked for with `-kithDemoScreens <name>`.
    public static let name: String? = {
        let args = ProcessInfo.processInfo.arguments
        guard let i = args.firstIndex(of: "-kithDemoScreens"), args.indices.contains(i + 1) else { return nil }
        return args[i + 1]
    }()
    public static var active: Bool { name != nil }
    /// The invite the `share-link` demo shows.
    public static var demoInvite: CreatedInvite? {
        let json: [String: Any] = ["inviteId": "i2", "link": "https://kith.app/join#demo-invite-token", "expiresAt": 0, "personId": "lee"]
        return (try? JSONSerialization.data(withJSONObject: json)).flatMap { try? JSONDecoder().decode(CreatedInvite.self, from: $0) }
    }
    /// The `bots-*` demos: one bot per state, and a second waiting one from Sam. Rex is Sam's active bot.
    static func demoBots(now: Int64) -> [BotGuest] {
        let hour: Int64 = 3_600_000
        return [
            BotGuest(botId: "bg_wait", name: "Rex", state: .pending, ownerPerson: "sam", ownerName: "Sam", agents: ["ada", "remy", "pip"],
                     createdAt: now - 50 * hour, joinedAt: now - 50 * hour),
            BotGuest(botId: "bg_echo", name: "Echo", provider: "other", state: .pending, agents: ["ada"], createdAt: now - 2 * hour,
                     joinedAt: now - 2 * hour),
            BotGuest(botId: "bg_act", name: "Rex Sr", state: .active, ownerPerson: "sam", ownerName: "Sam", agents: ["ada", "remy"],
                     limits: BotGuestLimits(ratePerHour: 60, concurrent: 2), connected: true, createdAt: now - 80 * hour,
                     joinedAt: now - 79 * hour, decidedAt: now - 78 * hour, decidedBy: "kai", lastSeenAt: now - 600_000, personId: "bot-1",
                     conversations: [BotGuestConversation(conversation: "c1", agent: "ada", chatBotId: DemoScreens.name?.hasPrefix("botguest") == true ? DemoScreens.botGuestChatId : nil,
                                                          threadRoot: DemoScreens.name?.hasPrefix("botguest") == true ? DemoScreens.botGuestRoot : nil,
                                                          originLabel: "Dinner plans", lastAt: now - hour)]),
            BotGuest(botId: "bg_inv", name: "Nova Bot", state: .invited, agents: ["pip"], createdAt: now - hour),
            BotGuest(botId: "bg_join", name: "Joiner", state: .joining, agents: ["pip"], createdAt: now - hour),
            BotGuest(botId: "bg_no", name: "Spam", state: .denied, ownerPerson: "maya", ownerName: "Maya", agents: ["ada"], createdAt: now - 90 * hour),
            BotGuest(botId: "bg_gone", name: "Old", state: .revoked, agents: ["ada"], createdAt: now - 99 * hour, revokedAt: now - 30 * hour),
        ]
    }

    /// The `botguest-*` demos: Rex (Sam's Grok bot) asked "personal"; its chat row and one conversation, shaped like a real host's.
    public static let botGuestChatId = "personal"
    public static let botGuestRoot = "bg-root"
    /// The bot the `chat` demo opens.
    public static let chatBotId = "coder"
    /// What the Crew dock starts with: `crew-typing` shows it mid-search.
    public static var query: String { name == "crew-typing" ? "co" : "" }
    /// The demos that show more than one computer: a Mac that's offline and one that turned the phone away.
    static var showsProblems: Bool { name == "problem" || name == "account-devices" }
    /// Simulator shots against a real host: `SIMCTL_CHILD_KITH_OPEN_<what>=<value>` opens that screen
    /// (`SPEND=architect` or `architect/limit`, `BOARD=1`, `SHARE=1`, `USAGE_SCROLL=spend`).
    public static func open(_ what: String) -> String? { ProcessInfo.processInfo.environment["KITH_OPEN_\(what)"] }
    #else
    public static func open(_ what: String) -> String? { nil }
    public static var active: Bool { false }
    public static let chatBotId = "coder"
    public static var name: String? { nil }
    public static var query: String { "" }
    #endif
}

#if DEBUG

/// A fixed, host-less account for CI screenshots (`-kithDemoScreens` in the app): one online computer with
/// a few bots in different states. Nothing here connects; the store is never made active.
extension AccountStore {
    @MainActor
    public static func demo(storage: SharedStore.Context) -> AccountStore {
        let key = Data((0..<32).map { UInt8($0 + 1) })
        let encoded = key.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        let mac = Computer(id: RelayCrypto.computerId(signKey: key), name: "Studio Mac", signKey: encoded, device: "macmini")
        storage.computers = [mac]
        // Problem demos: an offline Mac and one that stopped letting this iPhone in, beside the online one.
        func extra(_ name: String, _ device: String, byte: UInt8) -> Computer {
            let key = Data((0..<32).map { UInt8($0) &+ byte })
            let encoded = key.base64EncodedString().replacingOccurrences(of: "+", with: "-")
                .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
            return Computer(id: RelayCrypto.computerId(signKey: key), name: name, signKey: encoded, device: device)
        }
        let air = extra("MacBook Air", "macbook", byte: 40), mini = extra("Mac mini", "macmini", byte: 80)
        if DemoScreens.showsProblems { storage.computers += [air, mini] }
        let accounts = AccountStore(storage: storage, cloud: nil) { computer in
            // Never answers: the demo shows a fixed roster, not a connection.
            BotStore(computer: computer, storage: storage) {
                try await Task.sleep(for: .seconds(86_400))
                throw CancellationError()
            }
        }
        accounts.setActive(false)
        if DemoScreens.showsProblems {
            accounts.store(for: air.id)?.connection = .computerOffline(lastSeen: Date().addingTimeInterval(-3 * 3600))
            accounts.store(for: mini.id)?.connection = .unauthorized("Access was removed on the Mac.")
        }
        guard let store = accounts.store(for: mac.id) else { return accounts }
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        func bot(_ id: String, _ name: String, kind: String = "agent", status: String = "idle", unread: Int = 0,
                 last: String? = nil, minutesAgo: Int64 = 5, members: [String] = [], pinned: Bool = false,
                 avatar: String = "auto", lanes: [String] = []) -> Bot? {
            var json: [String: Any] = [
                "id": id, "name": name, "kind": kind, "rev": 1, "status": status, "unread": unread,
                "members": members, "pinned": pinned, "lastAt": now - minutesAgo * 60_000, "avatarSource": avatar,
            ]
            if kind == "agent" { json["hqAgent"] = id }
            if !lanes.isEmpty { json["workingLanes"] = lanes.map { ["chat": id, "thread": $0] } }
            if let last { json["lastMessage"] = last }
            return (try? JSONSerialization.data(withJSONObject: json)).flatMap { try? JSONDecoder().decode(Bot.self, from: $0) }
        }
        let bots = [
            bot("ada", "Ada", status: "needsInput", unread: 3, last: "Can I run the migration on staging?", minutesAgo: 2, pinned: true,
                avatar: "crew:coder"),
            bot("remy", "Remy", status: "working", last: "Reading the failing test output", minutesAgo: 1, avatar: "crew:designer",
                lanes: DemoScreens.name == "board" ? ["r1", "r2"] : []),
            bot("nova", "Nova", last: "Left off: run kith doctor next", minutesAgo: 180, avatar: "crew:image"),
            bot("pip", "Pip", unread: 1, last: "Draft is in the shared folder", minutesAgo: 45, avatar: "crew:house"),
            bot("team", "Release crew", kind: "group", unread: 2, last: "Ada: the build is green", minutesAgo: 12, members: ["ada", "remy", "pip"]),
        ].compactMap { $0 }
        for b in bots { store.bots[b.id] = b }
        seedChat(store, now: now)
        seedBoard(store, now: now)
        seedRoom(store, now: now)
        if DemoScreens.name?.hasPrefix("bots-") == true || DemoScreens.name?.hasPrefix("botguest") == true {
            store.botGuests.seedDemo(DemoScreens.demoBots(now: now))
        }
        if DemoScreens.name?.hasPrefix("botguest") == true { seedBotGuestChat(store, now: now) }
        store.connection = .online
        store.caughtUp = true
        return accounts
    }
}

extension AccountStore {
    /// The chat a bot from outside has with "personal", as a host sends it (`botGuest` on the row, one notice root with a thread).
    @MainActor
    static func seedBotGuestChat(_ store: BotStore, now: Int64) {
        func decode<T: Decodable>(_ json: [String: Any]) -> T? {
            (try? JSONSerialization.data(withJSONObject: json)).flatMap { try? JSONDecoder().decode(T.self, from: $0) }
        }
        let id = DemoScreens.botGuestChatId, root = DemoScreens.botGuestRoot
        let minute: Int64 = 60_000
        // `botguest-*-other` shows an outside bot that isn't Grok: the bubble disc.
        let other = DemoScreens.name?.hasSuffix("-other") == true
        let provider = other ? "other" : "grok", botName = other ? "Echo" : "Rex", tag = "from \(other ? "Other" : "Grok") · Sam's \(botName)"
        if let chat: Bot = decode([
            "id": id, "name": "Personal", "kind": "agent", "hqAgent": id, "rev": 1, "status": "idle", "unread": 1, "pinned": true, "members": [String](),
            "lastAt": now - 3 * minute, "avatarColor": "kith:sky", "backend": "claude", "cwd": "/Users/kai/kith", "avatarSource": "crew:house",
            "lastMessage": "You have two things today: dentist at 3 and dinner at 7.",
            "botGuest": ["botId": "bg_act", "name": botName, "provider": provider, "ownerPerson": "sam", "ownerName": "Sam"],
        ]) { store.bots[id] = chat }
        func entry(_ eid: String, _ seq: Int64, _ kind: String, thread: String? = nil, ago: Int64, data: [String: Any]) -> Entry? {
            var json: [String: Any] = ["id": eid, "seq": seq, "botId": id, "rev": seq, "kind": kind, "turn": seq, "data": data,
                                       "createdAt": now - ago * minute, "updatedAt": now - ago * minute]
            if let thread { json["threadId"] = thread }
            return decode(json)
        }
        let list = [
            entry(root, 1, "notice", ago: 4, data: ["text": tag, "style": "info", "conversation": "cv_demo",
                "thread": ["count": 2, "lastAt": now - 3 * minute, "authors": ["user", id], "unread": 1, "origin": "bot",
                           "originLabel": tag, "originBot": ["provider": provider, "owner": "sam", "name": botName]]]),
            entry("bg-q", 2, "user", thread: root, ago: 4, data: ["text": "What is on the calendar today?", "sender": ["botId": "bg_act", "name": botName, "speaker": "Alice"]]),
            entry("bg-a", 3, "agent", thread: root, ago: 3, data: ["text": "You have two things today: dentist at 3 and dinner at 7.", "final": true]),
        ].compactMap { $0 }
        store.entries[id] = list
        // A real host would say there is nothing earlier; without this the chat shows its "load earlier" spinner for good.
        store.historyComplete.insert(id)
    }

    /// Ada's question waits on every screen, so her row badge shows; the `board` demo adds the rest of the Board.
    @MainActor
    static func seedBoard(_ store: BotStore, now: Int64) {
        func decode<T: Decodable>(_ json: [String: Any]) -> T? {
            (try? JSONSerialization.data(withJSONObject: json)).flatMap { try? JSONDecoder().decode(T.self, from: $0) }
        }
        let minute: Int64 = 60_000
        var seq: Int64 = 0
        func entry(_ id: String, _ chat: String, _ kind: String, thread: String? = nil, ago: Int64, data: [String: Any]) -> Entry? {
            seq += 1
            var json: [String: Any] = ["id": id, "seq": seq, "botId": chat, "rev": seq, "kind": kind, "turn": seq,
                                       "data": data, "createdAt": now - ago * minute, "updatedAt": now - ago * minute]
            if let thread { json["threadId"] = thread }
            return decode(json)
        }
        func permission(_ id: String, _ title: String, ago: Int64) -> Entry? {
            entry(id, "ada", "permission", ago: ago, data: [
                "title": title, "toolKind": "execute", "status": "pending", "cwd": "/Users/kai/kith",
                "options": [["optionId": "o1", "name": "Allow once", "kind": "allow_once"],
                            ["optionId": "o3", "name": "Deny", "kind": "reject_once"]],
            ])
        }
        func task(_ id: String, _ chat: String, _ text: String, ago: Int64) -> Entry? {
            entry(id, chat, "user", ago: ago, data: ["text": text, "task": true,
                                                     "thread": ["count": 2, "lastAt": now - (ago - 1) * minute, "authors": [chat], "unread": 0]])
        }
        var ada = [permission("ap1", "psql -f migrate.sql", ago: 2)].compactMap { $0 }
        var remy: [Entry] = []
        var nova: [Entry] = []
        var pip: [Entry] = []
        if DemoScreens.name == "board" {
            ada += [permission("ap2", "cargo test -p kith-host", ago: 6)].compactMap { $0 }
            remy = [task("r1", "remy", "Fix the flaky sync test", ago: 20), task("r2", "remy", "Tidy the settings copy", ago: 12)].compactMap { $0 }
            if let done = task("n1", "nova", "Check the host health", ago: 90) {
                nova = [done, entry("n1a", "nova", "agent", thread: done.id, ago: 80, data: ["text": "All green: doctor passes.", "final": true]),
                        entry("n2", "nova", "notice", ago: 30, data: ["text": "Handoff", "handoff": ["status": "sent", "reason": "away", "text": "Left off: run kith doctor next, then check the sleep notice."]])]
                    .compactMap { $0 }
            }
            pip = [entry("p1", "pip", "user", ago: 3, data: ["text": "Can you also add the footer?", "status": "queued"])].compactMap { $0 }
        }
        // The `keys-*` demos: Access and Key cards in each state. Every name is an obvious fake and no card holds a
        // value: the host sends names and states only.
        if let name = DemoScreens.name, name.hasPrefix("keys-") {
            func secret(_ vars: [String], profile: String? = nil, replaces: Bool = false, state: String = "pending") -> [String: Any] {
                var out: [String: Any] = ["service": "github", "vars": vars, "purpose": "Open the pull request and read CI.",
                                          "replaces": replaces, "state": state]
                if let profile { out["profile"] = profile }
                return out
            }
            let accounts: [[String: Any]] = [["profile": "github/work", "account": "work", "agents": ["coder", "designer"]],
                                             ["profile": "github/personal", "account": "personal", "agents": ["personal"]]]
            let deny: [[String: Any]] = [["optionId": "deny", "name": "Not now", "kind": "reject_once"]]
            func access(_ id: String, ago: Int64, state: String = "pending", status: String = "pending", profile: String? = nil) -> Entry? {
                entry(id, "ada", "permission", ago: ago, data: ["card": "account", "status": status, "accounts": accounts, "options": deny,
                                                                "secret": secret(["GITHUB_TOKEN"], profile: profile, state: state)])
            }
            func key(_ id: String, ago: Int64, replaces: Bool = false, state: String = "pending", status: String = "pending", profile: String? = nil) -> Entry? {
                entry(id, "ada", "permission", ago: ago, data: ["card": "key", "status": status, "options": deny,
                                                                "secret": secret(["GITHUB_TOKEN"], profile: profile, replaces: replaces, state: state)])
            }
            let cards: [Entry?] = switch name {
            case "keys-access": [access("k1", ago: 1)]
            case "keys-replace": [key("k1", ago: 1, replaces: true, profile: "github/work")]
            case "keys-states": [access("k1", ago: 9, state: "saved", status: "answered", profile: "github/work"),
                                 key("k2", ago: 7, state: "saved", status: "answered", profile: "github/personal"),
                                 key("k3", ago: 5, state: "denied", status: "answered"),
                                 key("k4", ago: 3, state: "expired", status: "expired")]
            default: [key("k1", ago: 1)] // "keys-key", and "keys-board" for the Board row
            }
            ada = cards.compactMap { $0 }
        }
        store.entries["ada"] = ada
        store.entries["remy"] = remy
        store.entries["nova"] = nova
        store.entries["pip"] = pip
    }

    /// The `mermaid` demo's last reply: two diagrams and one mermaid can't draw, which stays a code block.
    private static let mermaidReply = """
    Both paths post it:

    ```mermaid
    sequenceDiagram
      Sweep->>Host: post notice
      Wake->>Host: post notice
    ```

    ```mermaid
    graph TD
      A -->
    ```

    ```mermaid
    graph LR
      W[Wake hook] --> N{Sent?}
      S[Sweep] --> N
      N -->|no| P[Post]
    ```
    """

    /// The `chat` demo: a bot with four tasks, one held by a terminal, and the newest with a tool row, a reply and a
    /// question waiting; the flat chat shows all their replies in time order.
    @MainActor
    static func seedChat(_ store: BotStore, now: Int64) {
        let id = DemoScreens.chatBotId
        func decode<T: Decodable>(_ json: [String: Any]) -> T? {
            (try? JSONSerialization.data(withJSONObject: json)).flatMap { try? JSONDecoder().decode(T.self, from: $0) }
        }
        let minute: Int64 = 60_000
        if let coder: Bot = decode([
            "id": id, "name": "Coder", "kind": "agent", "hqAgent": id, "rev": 1, "status": "idle", "unread": 0, "members": [String](),
            "pinned": false, "lastAt": now - 2 * minute, "avatarColor": "kith:sky", "backend": "claude", "cwd": "/Users/kai/kith",
            "folder": ["kind": "project", "project": "kith", "path": "/Users/kai/kith", "name": "kith"],
        ]) { store.bots[id] = coder }

        var seq: Int64 = 0
        func entry(_ kind: String, root: String? = nil, ago: Int64, data: [String: Any]) -> Entry? {
            seq += 1
            var json: [String: Any] = ["id": "d\(seq)", "seq": seq, "botId": id, "rev": seq, "kind": kind, "turn": seq,
                                       "data": data, "createdAt": now - ago * minute, "updatedAt": now - ago * minute]
            if let root { json["threadId"] = root }
            return decode(json)
        }
        func task(_ n: Int64, _ text: String, ago: Int64, thread: [String: Any]) -> Entry? {
            seq = n * 100
            return entry("user", ago: ago, data: ["text": text, "task": true, "thread": thread])
        }
        func summary(_ count: Int, ago: Int64, unread: Int = 0, extra: [String: Any] = [:]) -> [String: Any] {
            ["count": count, "lastAt": now - ago * minute, "authors": [id], "unread": unread].merging(extra) { $1 }
        }
        func reply(_ root: Entry?, ago: Int64, _ text: String) -> Entry? {
            entry("agent", root: root?.id, ago: ago, data: ["text": text, "final": true])
        }

        var list: [Entry] = []
        var threads: [String: PlaceFolder] = [:]
        let held = task(1, "Add --resume to the kith CLI", ago: 300, thread: summary(2, ago: 290, extra: [
            "origin": "terminal", "liveWriter": "terminal", "liveBusy": true,
        ]))
        let swap = task(2, "Why is swap so high on the mini?", ago: 200, thread: summary(2, ago: 190))
        let board = task(3, "Add the waits section to Board", ago: 90, thread: summary(5, ago: 60, unread: 1))
        let open = task(4, "The sleep notice fires twice, fix it", ago: 8, thread: summary(4, ago: 2))
        for root in [held, swap, board, open].compactMap({ $0 }) {
            list.append(root)
            threads[BotStore.threadFolderKey(id, root.id)] = PlaceFolder(kind: root.id == held?.id ? "folder" : "project",
                                                                         project: "kith", path: "/Users/kai/kith", name: root.id == held?.id ? "kith" : "kith")
        }
        if let held {
            list += [
                entry("notice", root: held.id, ago: 299, data: ["text": "Started in the terminal", "writerChange": "startedInTerminal"]),
                reply(held, ago: 290, "Added the flag; trying it against a saved session."),
            ].compactMap { $0 }
        }
        if let swap {
            list += [reply(swap, ago: 195, "Chrome is holding 6 GB of compressed pages. Quitting it frees most of the swap.")].compactMap { $0 }
        }
        if let board {
            list += [
                entry("tool", root: board.id, ago: 80, data: ["toolKind": "edit", "title": "BoardView.swift", "status": "completed",
                                                              "diffs": [["path": "BoardView.swift", "added": 42, "removed": 3, "isNew": false, "startLine": 1, "patch": "+waits"]]]),
                reply(board, ago: 60, "The waits section is in, below left off."),
            ].compactMap { $0 }
        }
        if let open {
            list += [
                reply(open, ago: 7, "Found it: the sweep and the wake hook both post the notice."),
                entry("tool", root: open.id, ago: 6, data: ["toolKind": "edit", "title": "kith-sessions.py", "status": "completed",
                                                            "diffs": [["path": "kith-sessions.py", "added": 6, "removed": 2, "isNew": false, "startLine": 1, "patch": "+skip"]]]),
                entry("tool", root: open.id, ago: 5, data: ["toolKind": "execute", "title": "swift test", "status": "completed", "output": "42 tests passed"]),
                reply(open, ago: 3, DemoScreens.name == "mermaid" ? mermaidReply : "One fix in two files. Want me to run the host tests too?"),
                entry("permission", root: open.id, ago: 2, data: [
                    "title": "cargo test -p kith-host", "toolKind": "execute", "status": "pending", "cwd": "/Users/kai/kith",
                    "options": [["optionId": "o1", "name": "Allow once", "kind": "allow_once"],
                                ["optionId": "o2", "name": "Always allow", "kind": "allow_always"],
                                ["optionId": "o3", "name": "Deny", "kind": "reject_once"]],
                ]),
            ].compactMap { $0 }
        }
        if DemoScreens.name == "news" {
            // The News card with real links, for the link-pictures shots (the simulator fetches their pictures).
            func row(_ title: String, _ line: String, _ source: String, _ domain: String, _ url: String) -> [String: Any] {
                ["title": title, "line": line, "source": source, "domain": domain, "url": url]
            }
            let rows = [
                row("Cold front brings snow to the Alps", "Up to 30 cm by Saturday, then clearing.", "BOM", "bom.gov.au", "https://www.bom.gov.au/"),
                row("Reserve Bank holds the cash rate", "Third meeting in a row without a move.", "ABC News", "abc.net.au", "https://www.abc.net.au/news"),
                row("What the new Swift concurrency checks catch", "A look at the Swift 6 migration.", "Swift.org", "swift.org", "https://www.swift.org/blog/"),
                row("Sydney ferry timetable changes from Monday", "Three routes move to a 20 minute gap.", "", "transportnsw.info", "https://transportnsw.info/"),
                row("A page with no picture", "Nothing to show here.", "Local", "example.com", "https://example.com/"),
            ]
            let card: [String: Any] = ["type": "news", "v": 1, "ref": ["agent": id, "job": "morning-brief"], "snapshot": [
                "name": "Morning brief", "every": "Every day at 7:00", "lead": "Five worth your time today.",
                "tail": "Want the long read summarised?", "rows": rows,
            ]]
            list = [entry("agent", ago: 1, data: ["text": "Morning brief: five items.", "final": true, "richCard": card])].compactMap { $0 }
            threads = [:]
        }
        store.entries[id] = list
        store.threadFolders.merge(threads) { $1 }
    }
}
#endif

#if DEBUG
/// The `share-*` demos: a fixed People list for the share sheet, so nothing talks to a host.
extension PeopleStore {
    @MainActor
    public static func demo() -> PeopleStore {
        PeopleStore(host: { DemoPeopleHost() })
    }
}

private struct DemoPeopleHost: PeopleHost {
    func people() async throws -> [HostPerson] {
        [HostPerson(personId: "maya", name: "Maya", agents: ["ada": .guest], joined: true),
         HostPerson(personId: "sam", name: "Sam", agents: ["ada": .member, "remy": .guest], joined: true),
         HostPerson(personId: "lee", name: "Lee", agents: ["ada": .guest], joined: false, openInvites: 1)]
    }
    func invites() async throws -> [HostInvite] {
        let soon = Int64((Date().timeIntervalSince1970 + 20 * 3600) * 1000)
        let json: [String: Any] = ["inviteId": "i1", "personId": "lee", "agents": ["ada": "guest"], "createdAt": 0, "expiresAt": soon]
        return [try JSONDecoder().decode(HostInvite.self, from: JSONSerialization.data(withJSONObject: json))]
    }
    func createInvite(name: String?, personId: String?, agents: [String: InviteRole]) async throws -> CreatedInvite {
        let json: [String: Any] = ["inviteId": "i2", "link": "https://kith.app/join#demo-invite-token", "expiresAt": 0]
        return try JSONDecoder().decode(CreatedInvite.self, from: JSONSerialization.data(withJSONObject: json))
    }
    func revokeInvite(_ inviteId: String) async throws {}
    func setPersonAccess(personId: String, agents: [String: InviteRole]?, projects: [String]?) async throws {}
    func revokePerson(_ personId: String) async throws {}
    func unrevokePerson(_ personId: String) async throws {}
    func audit(since: Date?, personId: String?) async throws -> [AuditRow] { [] }
    func projectList() async throws -> [HQProject] { [] }
}

#endif
