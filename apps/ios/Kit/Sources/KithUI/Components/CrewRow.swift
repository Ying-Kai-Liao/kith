import KithKit
import SwiftUI

/// A bot's face as plain values, so a row needs no store.
public struct CrewFace: Equatable, Sendable {
    public var portrait: String?
    public var color: String
    public var id: String

    public init(portrait: String?, color: String = "", id: String = "") {
        self.portrait = portrait
        self.color = color
        self.id = id
    }

    public init(bot: Bot) { self.init(portrait: CrewAvatar.asset(for: bot), color: bot.avatarColor, id: bot.id) }
}

/// Everything one Crew row shows. Plain values: the screen maps its store onto this.
public struct CrewRowModel: Identifiable, Equatable, Sendable {
    public enum Shape: Equatable, Sendable {
        /// Your chat with one agent.
        case agent(CrewFace)
        /// A room shared with a person, default agent: the agent's face, the person's sky on its lower left.
        case team(CrewFace, personId: String)
        /// A room with several agents: two bots on the diagonal and one sky patch. Named in Inter, never Nunito.
        case group(CrewFace, CrewFace, personId: String)
    }

    public var id: String
    public var shape: Shape
    public var name: String
    /// "with Mia" beside a team's agent name.
    public var withPerson: String?
    /// The time or state on the right: "working", "9:02", "asleep · 2 h".
    public var trailing: String?
    /// One sentence; nil hides the line.
    public var status: String?
    /// Questions waiting on you; 0 shows no badge.
    public var count: Int
    /// Unread messages and nothing waiting on you: a bold name and a small dot instead of a badge.
    public var unread: Bool
    public var state: BotAvatarState
    /// A bot from outside's chat: "from Grok · Sam's Rex", under the name, so it never reads as your own chat.
    public var tag: String?

    public init(id: String, shape: Shape, name: String, withPerson: String? = nil, trailing: String? = nil,
                status: String? = nil, count: Int = 0, unread: Bool = false, state: BotAvatarState = .idle, tag: String? = nil) {
        self.id = id
        self.shape = shape
        self.name = name
        self.withPerson = withPerson
        self.trailing = trailing
        self.status = status
        self.count = count
        self.unread = unread
        self.state = state
        self.tag = tag
    }
}

/// One row per room in Crew. Gutter 16, row padding 9; the screen draws separators and handles the tap.
public struct CrewRow: View {
    let model: CrewRowModel

    public init(_ model: CrewRowModel) { self.model = model }

    private var isGroup: Bool { if case .group = model.shape { true } else { false } }

    public var body: some View {
        HStack(spacing: 14) {
            CrewRowFace(shape: model.shape, state: model.state)
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(model.name)
                        .kithText(isGroup ? .topBar : .agentName)
                        .foregroundStyle(Palette.ink)
                        .fontWeight(showsDot ? .bold : nil)
                        .lineLimit(1)
                    if let person = model.withPerson {
                        Text("with \(person)").kithText(.body).foregroundStyle(Palette.muted).lineLimit(1)
                            .layoutPriority(-1)
                    }
                    Spacer(minLength: 6)
                    if let trailing = model.trailing {
                        Text(trailing).kithText(.secondary).foregroundStyle(Palette.muted).lineLimit(1).fixedSize()
                    }
                    if showsDot { Circle().fill(Palette.ink).frame(width: 8, height: 8) }
                    CountBadge(model.count)
                }
                if let tag = model.tag { OriginTag(.bot, label: tag) }
                if let status = model.status, !status.isEmpty {
                    Text(status).kithText(.body).foregroundStyle(Palette.mutedOnSky).lineLimit(1)
                }
            }
        }
        .padding(.horizontal, KithSpacing.gutter)
        .padding(.vertical, 13)
        .frame(minHeight: 64)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityText))
    }

    private var showsDot: Bool { model.unread && model.count == 0 }

    private var accessibilityText: String {
        [model.name, model.withPerson.map { "with \($0)" }, model.tag, model.trailing, model.status,
         model.count > 0 ? "\(model.count) waiting" : nil, model.unread ? "unread" : nil].compactMap { $0 }.joined(separator: ", ")
    }
}

/// The 54 pt mark of a row: agent, team or group.
struct CrewRowFace: View {
    let shape: CrewRowModel.Shape
    let state: BotAvatarState
    private let box: CGFloat = 54

    var body: some View {
        switch shape {
        case .agent(let face):
            bot(face, size: box)
        case .team(let face, let person):
            ZStack(alignment: .bottomLeading) {
                bot(face, size: box)
                SkyPatch(id: person, size: 22).offset(x: -4, y: 4)
            }
            .frame(width: box, height: box)
        case .group(let a, let b, let person):
            ZStack {
                bot(a, size: 34).position(x: 17, y: 17)
                bot(b, size: 32).position(x: 38, y: 37)
                SkyPatch(id: person, size: 22).position(x: 11, y: 43)
            }
            .frame(width: box, height: box)
        }
    }

    private func bot(_ face: CrewFace, size: CGFloat) -> some View {
        // Dots stay lower right, whatever else sits on the face.
        BotAvatar(portrait: face.portrait, color: face.color, id: face.id, size: size, state: size == box ? state : .idle, showsBadge: false)
    }
}

#Preview("Crew rows") {
    VStack(spacing: 0) {
        CrewRow(.init(id: "1", shape: .agent(.init(portrait: "crew-designer", id: "designer")), name: "designer",
                      status: "Which direction for the app?", count: 1, state: .needsYou(1)))
        CrewRow(.init(id: "2", shape: .group(.init(portrait: "crew-designer", id: "d"), .init(portrait: "crew-coder", id: "c"), personId: "mia"),
                      name: "Launch", status: "designer: Pick the tab icon set", count: 2))
        CrewRow(.init(id: "3", shape: .team(.init(portrait: "crew-coder", id: "coder"), personId: "mia"), name: "coder", withPerson: "Mia",
                      trailing: "working", status: "Relay fix · running tests", state: .working))
        CrewRow(.init(id: "4", shape: .agent(.init(portrait: "crew-builder", id: "builder")), name: "builder",
                      trailing: "asleep · 2 h", status: "Left off: run kith doctor next", state: .asleep))
        CrewRow(.init(id: "5", shape: .agent(.init(portrait: "crew-house", id: "house")), name: "A very long agent name that has to truncate somewhere",
                      trailing: "9:02", count: 120))
    }
}
