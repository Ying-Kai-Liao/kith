import KithKit
import SwiftUI

/// What hangs under your bubble: the thread's chip.
public struct ThreadChipModel: Equatable, Sendable {
    /// Where the thread ran, in small grey beside the chip.
    public enum Place: Equatable, Sendable {
        /// Inside a project: briefcase icon.
        case project(String)
        /// Outside any project: folder icon.
        case folder(String)

        var symbol: String {
            switch self {
            case .project: "briefcase"
            case .folder: "folder"
            }
        }

        var text: String {
            switch self {
            case .project(let t), .folder(let t): t
            }
        }
    }

    public var face: CrewFace
    public var replies: Int
    public var newReplies: Int
    public var time: String
    public var place: Place?
    /// "your turn": a plain dot and the words, in place of the bot and counts.
    public var yourTurn: Bool
    public var tint: AgentTint
    /// Where the thread started, when not in the app: a small tag on the meta line.
    public var origin: OriginTag.Origin?
    /// Who started a mirrored one-shot run; replaces the origin word.
    public var originLabel: String?
    /// A terminal holds the thread: the writer chip on the meta line.
    public var writer: StatusChip.Kind?

    public init(face: CrewFace, replies: Int, newReplies: Int = 0, time: String, place: Place? = nil, yourTurn: Bool = false, tint: AgentTint = .neutral,
                origin: OriginTag.Origin? = nil, originLabel: String? = nil, writer: StatusChip.Kind? = nil) {
        self.origin = origin
        self.originLabel = originLabel
        self.writer = writer
        self.face = face
        self.replies = replies
        self.newReplies = newReplies
        self.time = time
        self.place = place
        self.yourTurn = yourTurn
        self.tint = tint
    }

    var repliesText: String { replies == 1 ? "1 reply" : "\(replies) replies" }
}

/// One turn: your message as a right bubble, or the agent's as a left bubble. A turn of yours with a `thread`
/// shows the chip under it; `isOpen` unfolds `replies` in place with a thin line in the agent's colour pair.
public struct TranscriptTurn<Replies: View>: View {
    public enum Speaker: Sendable { case you, agent }

    let speaker: Speaker
    let text: String
    let thread: ThreadChipModel?
    let isOpen: Bool
    let onToggle: () -> Void
    let onOpenThread: () -> Void
    let replies: Replies

    public init(_ speaker: Speaker, text: String, thread: ThreadChipModel? = nil, isOpen: Bool = false,
                onToggle: @escaping () -> Void = {}, onOpenThread: @escaping () -> Void = {},
                @ViewBuilder replies: () -> Replies) {
        self.speaker = speaker
        self.text = text
        self.thread = thread
        self.isOpen = isOpen
        self.onToggle = onToggle
        self.onOpenThread = onOpenThread
        self.replies = replies()
    }

    public var body: some View {
        VStack(alignment: speaker == .you ? .trailing : .leading, spacing: 6) {
            bubble
            if let thread, speaker == .you {
                ThreadChipView(thread, isOpen: isOpen, onToggle: onToggle, onOpenThread: onOpenThread)
                if isOpen {
                    HStack(alignment: .top, spacing: 10) {
                        RoundedRectangle(cornerRadius: 1).fill(thread.tint.a).frame(width: 2)
                        VStack(alignment: .leading, spacing: 8) { replies }
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(.top, 4)
                    .transition(.opacity)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: speaker == .you ? .trailing : .leading)
    }

    private var bubble: some View {
        Text(text)
            .kithText(.body)
            .foregroundStyle(Palette.ink)
            .padding(.horizontal, 14).padding(.vertical, 10)
            .background(speaker == .you ? Palette.bubbleUser : Palette.bubbleAgent,
                        in: RoundedRectangle(cornerRadius: KithRadius.card + 4, style: .continuous))
            .frame(maxWidth: .infinity, alignment: speaker == .you ? .trailing : .leading)
            .padding(speaker == .you ? .leading : .trailing, 56)
    }
}

/// The thread chip on its own: place in grey, then the pill (bot, "N replies", "1 new", time) and the chevron.
/// `TranscriptTurn` draws it under your bubble; the chat screen draws it under bubbles it renders itself.
public struct ThreadChipView: View {
    let t: ThreadChipModel
    let isOpen: Bool
    let onToggle: () -> Void
    let onOpenThread: () -> Void
    /// Replaces the spoken label of the chip's main button (the chevron keeps its own).
    let openLabel: String?

    public init(_ model: ThreadChipModel, isOpen: Bool = false, openLabel: String? = nil,
                onToggle: @escaping () -> Void = {}, onOpenThread: @escaping () -> Void = {}) {
        t = model
        self.openLabel = openLabel
        self.isOpen = isOpen
        self.onToggle = onToggle
        self.onOpenThread = onOpenThread
    }

    public var body: some View {
        HStack(spacing: 8) {
            if let origin = t.origin { OriginTag(origin, label: t.originLabel) }
            if let writer = t.writer { StatusChip(writer, tint: t.tint) }
            if let place = t.place {
                Label { Text(place.text).lineLimit(1) } icon: { Image(systemName: place.symbol) }
                    .labelStyle(PlaceLabelStyle())
                    .kithText(.secondary).foregroundStyle(Palette.muted)
            }
            HStack(spacing: 0) {
                Button(action: onOpenThread) {
                    HStack(spacing: 6) {
                        if t.yourTurn {
                            Circle().fill(Palette.ink).frame(width: 7, height: 7)
                            Text("your turn").kithText(.secondary).fontWeight(.semibold).foregroundStyle(Palette.ink)
                        } else {
                            BotAvatar(portrait: t.face.portrait, color: t.face.color, id: t.face.id, size: KithSize.botAvatarThread)
                            Text(t.repliesText).kithText(.secondary).fontWeight(.semibold).foregroundStyle(Palette.ink)
                            if t.newReplies > 0 {
                                Text("\(t.newReplies) new").kithText(.secondary).fontWeight(.semibold).lineLimit(1).fixedSize()
                                    .foregroundStyle(Palette.ink)
                                    .padding(.horizontal, 7).padding(.vertical, 1)
                                    .background(t.tint.b, in: Capsule())
                            }
                        }
                        Text(t.time).kithText(.secondary).foregroundStyle(Palette.muted)
                    }
                    .padding(.leading, 8)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(openLabel.map { Text($0) } ?? Text(t.yourTurn ? "your turn" : t.repliesText))
                Button(action: onToggle) {
                    Image(systemName: isOpen ? "chevron.up" : "chevron.down")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .frame(width: 44, height: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text(isOpen ? "Fold replies" : "Show replies"))
            }
            // Visually the chip is 32 pt; the buttons inside keep a 44 pt target.
            .padding(.vertical, -6)
            .background(isOpen ? t.tint.b.opacity(0.55) : Palette.panel, in: Capsule())
        }
        .padding(.vertical, 2)
    }
}

private struct PlaceLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 4) {
            configuration.icon.font(.system(size: 11))
            configuration.title
        }
    }
}

public extension TranscriptTurn where Replies == EmptyView {
    // Disfavoured: a trailing closure after `onToggle` is the replies, not `onOpenThread`.
    @_disfavoredOverload
    init(_ speaker: Speaker, text: String, thread: ThreadChipModel? = nil, isOpen: Bool = false,
         onToggle: @escaping () -> Void = {}, onOpenThread: @escaping () -> Void = {}) {
        self.init(speaker, text: text, thread: thread, isOpen: isOpen, onToggle: onToggle, onOpenThread: onOpenThread) { EmptyView() }
    }
}

#Preview("Transcript turns") {
    let coder = CrewFace(portrait: "crew-coder", color: "kith:sky", id: "coder")
    let tint = AgentTint.resolve("kith:sky")
    return VStack(spacing: 14) {
        TranscriptTurn(.you, text: "Why is swap so high on the mini?",
                       thread: .init(face: coder, replies: 2, time: "8:14", place: .folder("kith"), tint: tint))
        TranscriptTurn(.you, text: "Add the waits section to Board",
                       thread: .init(face: coder, replies: 5, newReplies: 1, time: "8:52", place: .project("kith-app"), tint: tint))
        TranscriptTurn(.you, text: "The sleep notice fires twice, fix it",
                       thread: .init(face: coder, replies: 3, time: "now", place: .project("kith · serve/host"), tint: tint), isOpen: true) {
            TranscriptTurn(.agent, text: "Found it: the sweep and the wake hook both post the notice.")
            ToolCallRow(.init(verb: "Edited", object: "kith-sessions.py", result: .text("+6 -2")))
        }
        TranscriptTurn(.you, text: "Which icon set?", thread: .init(face: coder, replies: 1, time: "9:40", yourTurn: true, tint: tint))
    }
    .padding()
}
