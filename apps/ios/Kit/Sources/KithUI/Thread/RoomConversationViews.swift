import KithKit
import SwiftUI

// The agent-conversation pieces of a group room (docs/design: agent-conversations): the Bot and Late tags, the turns
// chip and its popover, the live line above the box, and the lines where a run ends. Words: "Bot" for one a person
// brought in, "turns" for what a message gives the room.

/// The small outlined capsule after a bot's name: marks one someone brought in, as opposed to Kith's own agents.
struct BotTag: View {
    var body: some View {
        Text("Bot")
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(Palette.muted)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .overlay(Capsule().strokeBorder(Palette.border, lineWidth: 1))
            .accessibilityLabel("Bot")
    }
}

/// "Late" beside the name of a reply that came after its run was over. A shape and a word, never a colour.
struct LateTag: View {
    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "clock").font(.system(size: 11, weight: .medium))
            Text("Late").font(.system(size: 13, weight: .medium))
        }
        .foregroundStyle(Palette.muted)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Late reply")
    }
}

/// An outside bot's face: the shipped disc for its provider.
struct RoomBotFace: View {
    let provider: String?
    let name: String
    var size: CGFloat = 24

    var body: some View {
        BotGuestAvatar(provider: provider ?? (name.lowercased() == "grok" ? "grok" : "other"), size: size)
            .shadow(color: Palette.ink.opacity(0.12), radius: 4, y: 2)
    }
}

/// The name line over a bot's message: face, name in Inter (agents keep Nunito) and the Bot capsule, then Late.
struct RoomBotLabel: View {
    let sender: EntrySender
    var late = false

    private var name: String { sender.name?.isEmpty == false ? sender.name! : "Bot" }

    var body: some View {
        HStack(spacing: 8) {
            RoomBotFace(provider: sender.provider, name: name)
            Text(name).kithText(.topBar).foregroundStyle(Palette.ink).lineLimit(1)
            BotTag()
            if late { LateTag() }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text("\(name), bot\(late ? ", late" : "")"))
    }
}

/// The bubble of a late reply: outlined, not filled, with what it answers over the words. No red, no dimming.
struct LateBubble<Content: View>: View {
    /// The message it answers, first line; nil when that message isn't known.
    let quote: String?
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let quote {
                HStack(spacing: 6) {
                    Image(systemName: "arrowshape.turn.up.left").font(.system(size: 12))
                    Text(quote).kithText(.secondary).lineLimit(1)
                }
                .foregroundStyle(Palette.muted)
            }
            content
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(Palette.background, in: RoundedRectangle(cornerRadius: KithRadius.card + 4, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: KithRadius.card + 4, style: .continuous).strokeBorder(Palette.border, lineWidth: 1))
    }
}

/// The one muted line under a late reply; nothing for a reason this version doesn't know.
struct LateReasonLine: View {
    let reason: LateWhy?

    var body: some View {
        if let reason {
            Text(reason.line).kithText(.secondary).foregroundStyle(Palette.muted)
        }
    }
}

// MARK: turns

/// "↻ 10" before Send in a group: quiet while it is the standard number, ink once changed. Tapping opens the popover.
struct TurnsChip: View {
    @Binding var count: Int
    @State private var open = false

    private var changed: Bool { count != Turns.standard }

    var body: some View {
        Button { open = true } label: {
            HStack(spacing: 4) {
                Image(systemName: "arrow.clockwise").font(.system(size: 11, weight: .semibold))
                Text("\(count)").font(.system(size: 14, weight: .semibold)).monospacedDigit()
            }
            .foregroundStyle(changed ? Palette.background : Palette.muted)
            .padding(.horizontal, 11)
            .frame(height: 32)
            .background(changed ? Palette.ink : Palette.ink.opacity(0.06), in: Capsule())
            // The tap target is the 44 pt the dock's buttons share.
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // The `room-turns` demo shot opens it.
        .task { if DemoScreens.name == "room-turns" { try? await Task.sleep(for: .seconds(1)); open = true } }
        .accessibilityLabel("Turns: \(count)")
        .accessibilityHint("How many turns this message gives them")
        .popover(isPresented: $open, attachmentAnchor: .point(.top), arrowEdge: .bottom) {
            TurnsPopover(count: $count)
                .presentationCompactAdaptation(.popover)
        }
    }
}

/// "Let them work it out": a stepper and the picks. The number applies to the message being written, nothing else.
struct TurnsPopover: View {
    @Binding var count: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Let them work it out").kithText(.topBar).foregroundStyle(Palette.ink)
            HStack {
                stepButton("minus", label: "Fewer turns", enabled: count > Turns.minimum) { count = Turns.clamp(count - 1) }
                Spacer(minLength: 0)
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text("\(count)").font(.system(size: 34, weight: .semibold)).monospacedDigit()
                        .foregroundStyle(Palette.ink)
                        .contentTransition(.numericText())
                    Text(count == 1 ? "turn" : "turns").kithText(.body).foregroundStyle(Palette.muted)
                }
                Spacer(minLength: 0)
                stepButton("plus", label: "More turns", enabled: count < Turns.maximum) { count = Turns.clamp(count + 1) }
            }
            .padding(8)
            .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).strokeBorder(Palette.border, lineWidth: 1))
            HStack(spacing: 6) {
                ForEach(Turns.picks, id: \.self) { pick in
                    Button { count = pick } label: {
                        Text("\(pick)").font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(count == pick ? Palette.background : Palette.ink)
                            .frame(maxWidth: .infinity, minHeight: 36)
                            .background(count == pick ? Palette.ink : Color.clear, in: Capsule())
                            .overlay(Capsule().strokeBorder(count == pick ? Color.clear : Palette.border, lineWidth: 1))
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("\(Turns.words(pick))")
                    .accessibilityAddTraits(count == pick ? .isSelected : [])
                }
            }
            Text("Each reply or pass is one turn. You can stop them any time.")
                .kithText(.secondary).foregroundStyle(Palette.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(18)
        .frame(width: 320)
        .animation(.snappy(duration: 0.18), value: count)
    }

    private func stepButton(_ icon: String, label: String, enabled: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon).font(.system(size: 16, weight: .semibold)).foregroundStyle(Palette.ink)
                .frame(width: 44, height: 44)
                .background(Palette.ink.opacity(0.06), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.35)
        .accessibilityLabel(label)
    }
}

// MARK: the live line and where a run ends

/// One glass line above the box while the room works it out: whose turn, how many turns are left, and Stop.
struct RoomLiveLineView: View {
    let conversation: RoomConversation
    let stop: () -> Void
    @Environment(BotStore.self) private var model

    var body: some View {
        // The wait of an outside bot's turn counts down in minutes: look again now and then.
        TimelineView(.periodic(from: .now, by: 15)) { context in
            if let line = conversation.liveLine(now: context.date) {
                content(line)
            }
        }
    }

    private func content(_ line: RoomLiveLine) -> some View {
        HStack(spacing: 10) {
            face
            VStack(alignment: .leading, spacing: 4) {
                Text(line.title).kithText(.topBar).foregroundStyle(Palette.ink).lineLimit(1)
                HStack(spacing: 8) {
                    Capsule().fill(Palette.ink.opacity(0.12)).frame(width: 44, height: 3)
                        .overlay(alignment: .leading) {
                            Capsule().fill(Palette.ink.opacity(0.55)).frame(width: 44 * line.fraction, height: 3)
                        }
                    Text(line.detail).kithText(.secondary).foregroundStyle(Palette.muted).lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            Button(action: stop) {
                HStack(spacing: 6) {
                    Image(systemName: "stop.fill").font(.system(size: 10, weight: .bold))
                    Text("Stop").font(.system(size: 15, weight: .semibold))
                }
                .foregroundStyle(Palette.background)
                .padding(.horizontal, 16)
                .frame(minHeight: 44)
                .background(Palette.ink, in: Capsule())
                .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Stop")
        }
        .padding(.leading, 14)
        .padding(.trailing, 8)
        .padding(.vertical, 8)
        .kithGlass(in: RoundedRectangle(cornerRadius: KithRadius.glassPill, style: .continuous))
        .padding(.horizontal, KithSpacing.gutter)
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder private var face: some View {
        if let speaker = conversation.current {
            switch speaker.kind {
            case .bot:
                RoomBotFace(provider: model.botGuests.bot(speaker.id)?.provider, name: speaker.name, size: 32)
            case .agent:
                if let bot = model.bots[speaker.id] { CharacterAvatar(bot: bot, size: 32, animated: false) } else { noFace }
            }
        } else {
            noFace
        }
    }

    private var noFace: some View {
        Image(systemName: "ellipsis.bubble").font(.system(size: 18)).foregroundStyle(Palette.muted).frame(width: 32, height: 32)
    }
}

/// Where a run ends: a quiet line, and after the turns ran out, the button that gives the room more.
struct RoomEndLineView: View {
    let line: RoomEndLine
    /// Nil when the line is an old one: only the newest run offers more.
    var more: (() -> Void)?

    var body: some View {
        VStack(spacing: 10) {
            HStack(spacing: 10) {
                Rectangle().fill(Palette.border).frame(height: 1)
                Text(line.text).kithText(.secondary).foregroundStyle(Palette.muted)
                    .multilineTextAlignment(.center).fixedSize(horizontal: false, vertical: true).layoutPriority(1)
                Rectangle().fill(Palette.border).frame(height: 1)
            }
            if line.offersMore, let more {
                Button(action: more) {
                    Label(RoomEndLine.moreLabel, systemImage: "arrow.clockwise")
                        .font(.system(size: 15, weight: .semibold)).foregroundStyle(Palette.ink)
                        .padding(.horizontal, 16).frame(minHeight: 44)
                        .overlay(Capsule().strokeBorder(Palette.border, lineWidth: 1))
                        .contentShape(Capsule())
                }
                .buttonStyle(PressScale())
            }
        }
        .padding(.top, 14)
        .padding(.bottom, 4)
    }
}

/// A bot joining or leaving, passing or not answering: a quiet line in the room, in the host's words.
struct RoomNoticeLine: View {
    let entry: Entry

    var body: some View {
        let text = entry.data.text ?? ""
        switch entry.data.notice {
        case "botJoined", "botLeft":
            HStack(spacing: 10) {
                Rectangle().fill(Palette.border).frame(height: 1)
                Text(text).kithText(.secondary).foregroundStyle(Palette.muted)
                    .multilineTextAlignment(.center).fixedSize(horizontal: false, vertical: true).layoutPriority(1)
                Rectangle().fill(Palette.border).frame(height: 1)
            }
            .padding(.vertical, 6)
        default:
            Text(text).kithText(.secondary).foregroundStyle(Palette.muted)
                .multilineTextAlignment(.center).frame(maxWidth: .infinity)
        }
    }
}
