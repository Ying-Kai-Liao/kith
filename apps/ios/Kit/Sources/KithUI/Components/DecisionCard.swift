import KithKit
import SwiftUI

/// One button of a decision card.
public struct DecisionOption: Identifiable, Equatable, Sendable {
    public enum Role: Equatable, Sendable { case primary, normal, destructive }
    public var id: String
    public var title: String
    public var role: Role

    public init(id: String, title: String, role: Role = .normal) {
        self.id = id
        self.title = title
        self.role = role
    }
}

/// An agent's question, or a permission request, as plain values.
public struct DecisionCardModel: Equatable, Sendable {
    public var heading: String?
    public var question: String
    /// The command or file the agent wants to touch, in mono.
    public var detail: String?
    public var options: [DecisionOption]
    /// A bot from outside's request: its provider, for the avatar before the heading.
    public var botProvider: String?

    public init(heading: String? = nil, question: String, detail: String? = nil, options: [DecisionOption]) {
        self.heading = heading
        self.question = question
        self.detail = detail
        self.options = options
    }

    /// Allow / Deny / Always. Option ids are the caller's (the host's option ids).
    public static func permission(request: String, detail: String? = nil, allow: String = "allow", deny: String = "deny",
                                  always: String = "always") -> DecisionCardModel {
        .init(question: request, detail: detail, options: [
            .init(id: allow, title: "Allow", role: .primary),
            .init(id: deny, title: "Deny", role: .destructive),
            .init(id: always, title: "Always"),
        ])
    }

    /// The owner's view of a guest's prompt: "<person> · <agent>" over what it wants, with the options it offered
    /// (ids are the host's `optionId`s).
    public init(guest prompt: GuestPrompt) {
        self.init(heading: prompt.who, question: prompt.request.isEmpty ? "Wants to do something" : prompt.request,
                  options: prompt.options.map { option in
            DecisionOption(id: option.optionId, title: GuestPrompt.label(option, edge: prompt.entry.data.edge),
                           role: option.kind == "allow_once" ? .primary : option.kind.hasPrefix("allow") ? .normal : .destructive)
        })
    }

    /// The owner's view of a bot from outside asking to join: "Sam's bot · Grok" over "Rex wants to ask house and
    /// notes". Option ids are `allow`, `deny` and `always`; the last only for a bot an invited person made.
    public init(botRequest bot: BotGuest) {
        var options = [
            DecisionOption(id: "allow", title: "Allow", role: .primary),
            DecisionOption(id: "deny", title: "Deny", role: .destructive),
        ]
        if bot.ownerPerson != nil {
            let who = bot.ownerName?.isEmpty == false ? "\(bot.ownerName!)'s" : "their"
            options.append(DecisionOption(id: "always", title: "Always allow \(who) bots"))
        }
        self.init(heading: bot.requestHeading, question: bot.requestQuestion, options: options)
        botProvider = bot.provider
    }

    /// Short answers sit in one row; anything longer stacks so labels wrap instead of clipping.
    var fitsInRow: Bool { options.count <= 3 && options.allSatisfy { $0.title.count <= 12 } }
}

/// An agent's question as buttons, tinted with that agent's colour pair at low strength. No amber outline.
///
/// The callback fires once per tap and the card does not remember it: the caller must guard a double tap (set
/// `answering` as soon as `onChoose` runs; while it is non-nil every button is disabled and the chosen one shows
/// a spinner).
public struct DecisionCard: View {
    let model: DecisionCardModel
    let tint: AgentTint
    let answering: String?
    let onChoose: (String) -> Void

    public init(_ model: DecisionCardModel, tint: AgentTint = .neutral, answering: String? = nil, onChoose: @escaping (String) -> Void) {
        self.model = model
        self.tint = tint
        self.answering = answering
        self.onChoose = onChoose
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let provider = model.botProvider {
                HStack(spacing: 8) {
                    BotGuestAvatar(provider: provider)
                    if let heading = model.heading { Text(heading).kithText(.label).foregroundStyle(Palette.muted) }
                }
            } else if let heading = model.heading { Text(heading).kithText(.label).foregroundStyle(Palette.muted) }
            Text(model.question).kithText(.body).fontWeight(.semibold).foregroundStyle(Palette.ink)
            if let detail = model.detail, !detail.isEmpty {
                Text(detail).kithText(.code).foregroundStyle(Palette.muted).lineLimit(4)
            }
            if model.fitsInRow {
                // One row when every label fits on one line; otherwise stacked, never wrapped mid-label.
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) { buttons(singleLine: true) }
                    VStack(spacing: 8) { buttons(singleLine: false) }
                }
            } else {
                VStack(spacing: 8) { buttons(singleLine: false) }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(tint.b.opacity(0.32), in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
        .accessibilityElement(children: .contain)
    }

    private func buttons(singleLine: Bool) -> some View {
        ForEach(model.options) { option in
            Button { onChoose(option.id) } label: {
                HStack(spacing: 6) {
                    Text(option.title)
                        .kithText(.body).fontWeight(.semibold)
                        .multilineTextAlignment(.center)
                        .lineLimit(singleLine ? 1 : nil)
                        .fixedSize(horizontal: singleLine, vertical: true)
                    if answering == option.id { ProgressView().controlSize(.mini) }
                }
                .foregroundStyle(foreground(option.role))
                .frame(maxWidth: .infinity, minHeight: 44)
                .padding(.horizontal, 12)
                .background(fill(option.role), in: RoundedRectangle(cornerRadius: KithRadius.rowTool + 4, style: .continuous))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(answering != nil)
            .opacity(answering == nil || answering == option.id ? 1 : 0.4)
        }
    }

    private func foreground(_ role: DecisionOption.Role) -> Color {
        switch role {
        case .primary: Palette.background
        case .normal: Palette.ink
        case .destructive: Palette.negative
        }
    }

    private func fill(_ role: DecisionOption.Role) -> Color {
        role == .primary ? Palette.ink : Palette.background.opacity(0.85)
    }
}

#Preview("Decision card, tinted") {
    VStack(spacing: 14) {
        DecisionCard(.init(question: "Which direction for the app?", options: [
            .init(id: "a", title: "Sky", role: .primary), .init(id: "b", title: "Paper"), .init(id: "c", title: "Night"),
        ]), tint: AgentTint.resolve("kith:coral")) { _ in }
        DecisionCard(.init(question: "Open a PR now, or batch it with the relay fix and the long sleep notice follow-up?", options: [
            .init(id: "a", title: "Open a PR now", role: .primary),
            .init(id: "b", title: "Batch it with the relay fix and wait for review of the whole thing"),
        ]), tint: AgentTint.resolve("kith:sky")) { _ in }
    }
    .padding()
}

#Preview("Decision card, permission") {
    DecisionCard(.permission(request: "Run a shell command?", detail: "swift test --filter KithUITests"),
                 tint: AgentTint.resolve("kith:mint")) { _ in }
        .padding()
}
