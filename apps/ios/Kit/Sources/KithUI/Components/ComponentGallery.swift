#if DEBUG
import KithKit
import SwiftUI

/// Every redesign component and state on one screen, for the designer and for screenshots. Debug builds open it
/// with the launch argument `-kithComponentGallery`; add `-kithGallerySection <name>` (avatars, rows, thread,
/// cards, dock, bar, terminal) to show one section at the top.
public struct KithComponentGallery: View {
    @State private var tab = HomeTab.crew
    @State private var open = true
    @State private var live = ""
    private let section: String?

    public init() {
        KithUIFonts.register()
        let args = ProcessInfo.processInfo.arguments
        section = args.firstIndex(of: "-kithGallerySection").flatMap { args.indices.contains($0 + 1) ? args[$0 + 1] : nil }
    }

    private let coder = CrewFace(portrait: "crew-coder", color: "kith:sky", id: "coder")
    private let designer = CrewFace(portrait: "crew-designer", color: "kith:coral", id: "designer")

    public var body: some View {
        ZStack(alignment: .top) {
            SkyBackground(.home)
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    if section == nil || section == "bar" { bar }
                    if section == nil || section == "avatars" { avatars }
                    if section == nil || section == "rows" { rows }
                    if section == nil || section == "thread" { thread }
                    if section == nil || section == "cards" { cards }
                    if section == nil || section == "dock" { dock }
                    if section == nil || section == "terminal" { terminal }
                }
                .padding(.top, 54).padding(.bottom, 40)
            }
        }
        .background(Palette.background)
    }

    private func block<C: View>(_ title: String, @ViewBuilder _ content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title.uppercased()).kithText(.label).foregroundStyle(Palette.mutedOnSky).padding(.horizontal, 16)
            content()
        }
    }

    private var bar: some View {
        block("Top bar and chips") {
            HStack {
                ProfileTopButton(action: {}) { SkyPatch(id: "kai", size: 38, isYou: true) }
                Spacer()
                HomeTabSwitch(selection: $tab)
                Spacer()
                TopGlassMenu("New chat") { Button("New bot", systemImage: "plus.square") {} }
            }.padding(.horizontal, 16)
            FlowChips()
            HStack(spacing: 8) {
                TopBarText("Top bar text, Inter 600 16")
                CountBadge(1); CountBadge(12); CountBadge(100); CountBadge(3, size: KithSize.countBadgeBoard)
            }.padding(.horizontal, 16)
        }
    }

    private var avatars: some View {
        block("Bot avatar: idle, working, needs you, asleep") {
            HStack(spacing: 20) {
                BotAvatar(portrait: coder.portrait, color: coder.color, id: "coder", state: .idle)
                BotAvatar(portrait: coder.portrait, color: coder.color, id: "coder2", state: .working)
                BotAvatar(portrait: designer.portrait, color: designer.color, id: "designer", state: .needsYou(2))
                BotAvatar(portrait: "crew-builder", id: "builder", state: .asleep)
                BotAvatar(portrait: "crew-house", color: "blue", id: "house")
            }.padding(.horizontal, 16)
            HStack(spacing: 8) {
                BotAvatar(portrait: "crew-house", id: "h1", size: KithSize.botAvatarColourSheet)
                BotAvatar(portrait: "crew-house", id: "h2", size: KithSize.botAvatarTopBar)
                BotAvatar(portrait: "crew-house", id: "h3", size: KithSize.botAvatarThread)
                SkyPatch(id: "kai", size: 48, isYou: true)
                SkyPatch(id: "mia", size: 48)
                SkyPatch(id: "ren", size: 48)
                SkyPatch(id: "lee", size: 30)
            }.padding(.horizontal, 16)
        }
    }

    private var rows: some View {
        block("Crew rows: agent, team, group") {
            VStack(spacing: 0) {
                CrewRow(.init(id: "1", shape: .agent(designer), name: "designer", status: "Which direction for the app?", count: 1, state: .needsYou(1)))
                CrewRow(.init(id: "2", shape: .group(designer, coder, personId: "mia"), name: "Launch", status: "designer: Pick the tab icon set", count: 2))
                CrewRow(.init(id: "3", shape: .team(coder, personId: "mia"), name: "coder", withPerson: "Mia", trailing: "working", status: "Relay fix · running tests", state: .working))
                CrewRow(.init(id: "4", shape: .agent(.init(portrait: "crew-builder", id: "builder")), name: "builder", trailing: "asleep · 2 h", status: "Left off: run kith doctor next", state: .asleep))
                CrewRow(.init(id: "5", shape: .agent(.init(portrait: "crew-house", id: "house")), name: "house with a very long name to truncate", trailing: "9:02", count: 120))
                CrewRow(.init(id: "6", shape: .agent(.init(portrait: "crew-image", id: "image")), name: "image", trailing: "Tuesday"))
            }
        }
    }

    private var thread: some View {
        let tint = AgentTint.resolve(coder.color)
        return block("Transcript turns, thread chips, tool rows") {
            VStack(spacing: 14) {
                TranscriptTurn(.you, text: "Why is swap so high on the mini?", thread: .init(face: coder, replies: 2, time: "8:14", place: .folder("kith"), tint: tint))
                TranscriptTurn(.you, text: "Add the waits section to Board", thread: .init(face: coder, replies: 5, newReplies: 1, time: "8:52", place: .project("kith-app"), tint: tint))
                TranscriptTurn(.you, text: "The sleep notice fires twice, fix it", thread: .init(face: coder, replies: 3, time: "now", place: .project("kith · serve/host"), tint: tint), isOpen: open, onToggle: { open.toggle() }) {
                    TranscriptTurn(.agent, text: "Found it: the sweep and the wake hook both post the notice. The hook now skips if the sweep sent one.")
                    ToolCallRow(.init(verb: "Edited", object: "kith-sessions.py", result: .text("+6  -2"), command: "edit kith-sessions.py", output: "ok"))
                    ToolCallRow(.init(verb: "Ran", object: "swift test"))
                    ToolCallRow(.init(verb: "Ran", object: "cargo build --release", result: .error("failed")))
                }
                TranscriptTurn(.you, text: "Which icon set?", thread: .init(face: designer, replies: 1, time: "9:40", yourTurn: true, tint: AgentTint.resolve(designer.color)))
            }.padding(.horizontal, 16)
        }
    }

    private var cards: some View {
        block("Handoff and decision cards") {
            VStack(spacing: 14) {
                HandoffCardView(.init(done: "Fixed the double sleep notice.", next: "Open a PR, or batch it with the relay fix.", files: ["kith-sessions.py"], prs: ["#112 sleep notice"]))
                HandoffCardView(.init(next: "Run kith doctor next."))
                DecisionCard(.init(question: "Which direction for the app?", options: [.init(id: "a", title: "Sky", role: .primary), .init(id: "b", title: "Paper"), .init(id: "c", title: "Night")]), tint: AgentTint.resolve(designer.color)) { _ in }
                DecisionCard(.permission(request: "Run a shell command?", detail: "swift test --filter KithUITests"), tint: AgentTint.resolve(coder.color)) { _ in }
                DecisionCard(.init(question: "Open a PR now, or batch it?", options: [.init(id: "a", title: "Open a PR now", role: .primary), .init(id: "b", title: "Batch it with the relay fix and wait for review of the whole thing")]), tint: AgentTint.resolve("kith:mint")) { _ in }
            }.padding(.horizontal, 16)
        }
    }

    private var terminal: some View {
        let tint = AgentTint.resolve(coder.color)
        return block("Terminal: banner states, long Mac name, chips, origin tags, dividers") {
            VStack(alignment: .leading, spacing: 10) {
                WriterBanner(phase: .idle, mac: "Mac mini", tint: tint)
                WriterBanner(phase: .working, mac: "Mac mini", tint: tint)
                WriterBanner(phase: .taking, mac: "Mac mini", tint: tint, pending: true)
                WriterBanner(phase: .failed, mac: "Mac mini", tint: tint)
                WriterBanner(phase: .unreachable, mac: "Mac mini", tint: tint)
                WriterBanner(phase: .idle, mac: "Kai's MacBook Pro in the upstairs office with a very long name", tint: tint)
                WriterBanner(phase: .idle, mac: "Mia's MacBook", mine: false, person: "Mia", tint: tint)
                HStack {
                    StatusChip(.writer(working: false, mac: "Mac mini"), tint: tint)
                    StatusChip(.writer(working: true, mac: nil), tint: tint)
                }
                HStack(spacing: 14) { ForEach(OriginTag.Origin.allCases, id: \.self) { OriginTag($0) } }
                WriterDivider(.startedInTerminal(mac: "Mac mini"), time: "9:40")
                WriterDivider(.openedInTerminal(mac: "Mac mini"), time: "10:31")
                WriterDivider(.tookOver, time: "10:02")
                WriterDivider(.terminalClosed, time: "10:48")
            }.padding(.horizontal, 16)
        }
    }

    private var dock: some View {
        block("Chat dock: empty, typing, agent chat with attachment, live") {
            VStack(spacing: 12) {
                ChatDock(text: .constant(""), placeholder: "Find or ask…")
                ChatDock(text: .constant("Why is the mini so slow?"), placeholder: "Find or ask…")
                ChatDock(text: .constant(""), placeholder: "Tell coder…", showsCall: false, hasAttachment: true) {
                    HStack { Label("screenshot.png", systemImage: "photo").kithText(.secondary).foregroundStyle(Palette.muted); Spacer() }
                        .padding(.horizontal, 14).padding(.top, 10)
                }
                ChatDock(text: $live, placeholder: "Type here: the call button slides away")
            }
        }
    }
}

private struct FlowChips: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack { StatusChip(.live); StatusChip(.catchingUp); StatusChip(.needsYou); StatusChip(.urgent) }
            HStack { StatusChip(.macUnreachable); StatusChip(.busy(computer: "Mac mini", queued: 1)) }
        }.padding(.horizontal, 16)
    }
}

#Preview("Component gallery") { KithComponentGallery() }
#endif
