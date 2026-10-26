import KithKit
import SwiftUI

/// A limit's bar: thin, in the agent's colours (or ink on a card); no bar means no limit.
public struct LimitBar: View {
    let value: Double
    let fill: AnyShapeStyle
    var height: CGFloat = 3

    public init(value: Double, tint: AgentTint, height: CGFloat = 3) {
        self.value = value
        fill = AnyShapeStyle(LinearGradient(colors: [tint.a, tint.b], startPoint: .leading, endPoint: .trailing))
        self.height = height
    }

    public init(value: Double, ink: Bool, height: CGFloat = 4) {
        self.value = value
        fill = AnyShapeStyle(Palette.ink)
        self.height = height
    }

    public var body: some View {
        Capsule().fill(Palette.line).frame(height: height)
            .overlay(alignment: .leading) {
                GeometryReader { geo in
                    Capsule().fill(fill).frame(width: geo.size.width * min(max(value, 0), 1))
                }
            }
            .accessibilityHidden(true)
    }
}

/// An agent at its monthly limit (the owner's run waits under it), or a guest at their budget: the host's card
/// (serve/host/src/limits.rs), tinted in the agent's pair. The words are `LimitCardModel`'s; the answer is the store's.
public struct LimitCardView: View {
    let entry: Entry
    let tint: AgentTint
    let answering: String?
    /// Someone else answers it (a guest's own view): "Waiting for …" in place of the buttons.
    var waiting: String?
    var leading: AnyView?
    var time: String?
    var personName: String?
    /// Shown on Board, away from its chat.
    var board = false
    let respond: (String) -> Void

    public init(entry: Entry, tint: AgentTint, answering: String?, waiting: String? = nil, leading: AnyView? = nil, time: String? = nil,
                personName: String? = nil, board: Bool = false, respond: @escaping (String) -> Void) {
        self.entry = entry
        self.tint = tint
        self.answering = answering
        self.waiting = waiting
        self.leading = leading
        self.time = time
        self.personName = personName
        self.board = board
        self.respond = respond
    }

    public var body: some View {
        if let m = LimitCardModel(entry, personName: personName, board: board) {
            let pending = entry.data.status == "pending"
            VStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 6) {
                        if let leading { leading }
                        Text(m.heading).kithText(.label).fontWeight(.semibold).foregroundStyle(Palette.ink).lineLimit(1)
                        Spacer(minLength: 4)
                        if let time { Text(time).kithText(.label).monospacedDigit().foregroundStyle(Palette.muted) }
                    }
                    Text(m.title).kithText(.body).fontWeight(.semibold).foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack {
                        Text(m.left).foregroundStyle(Palette.muted)
                        Spacer(minLength: 8)
                        Text(m.right).monospacedDigit().foregroundStyle(Palette.ink)
                    }
                    .kithText(.secondary)
                    LimitBar(value: m.fullness, ink: true)
                    if !pending {
                        Text(Spend.outcome(entry)).kithText(.secondary).fontWeight(.medium).foregroundStyle(Palette.muted)
                    } else if let waiting {
                        Text("Waiting for \(waiting)").kithText(.secondary).fontWeight(.medium).foregroundStyle(Palette.muted)
                    } else {
                        buttons(m.options)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(14)
                .background(
                    LinearGradient(colors: [tint.a.opacity(0.16), tint.b.opacity(0.22)], startPoint: .topLeading, endPoint: .bottomTrailing),
                    in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous)
                )
                .overlay(RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous).strokeBorder(tint.b.opacity(0.3)))
                .accessibilityElement(children: .contain)
                if let footer = m.footer, pending, waiting == nil {
                    Text(footer).kithText(.secondary).foregroundStyle(Palette.muted).multilineTextAlignment(.center)
                }
            }
        }
    }

    /// The main answers as pills (the first in ink), then the quiet one (Stop, Leave it) as text.
    private func buttons(_ options: [LimitCardModel.Option]) -> some View {
        let main = options.filter { $0.tone != .plain }
        let quiet = options.filter { $0.tone == .plain }
        return ViewThatFits(in: .horizontal) {
            HStack(spacing: 8) {
                ForEach(main) { button($0) }
                Spacer(minLength: 0)
                ForEach(quiet) { button($0) }
            }
            VStack(alignment: .leading, spacing: 8) {
                ForEach(main) { button($0).frame(maxWidth: .infinity) }
                HStack { ForEach(quiet) { button($0) } }
            }
        }
        .padding(.top, 2)
    }

    private func button(_ o: LimitCardModel.Option) -> some View {
        Button { respond(o.id) } label: {
            HStack(spacing: 6) {
                Text(o.label).kithText(.body).fontWeight(.semibold).lineLimit(1)
                if answering == o.id { ProgressView().controlSize(.mini) }
            }
            .foregroundStyle(o.tone == .ink ? Palette.background : Palette.ink)
            .padding(.horizontal, o.tone == .plain ? 6 : 14)
            .frame(minHeight: 40)
            .background {
                switch o.tone {
                case .ink: Capsule().fill(Palette.ink)
                case .pair: Capsule().fill(Palette.background.opacity(0.85))
                case .plain: Color.clear
                }
            }
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .disabled(answering != nil)
        .opacity(answering == nil || answering == o.id ? 1 : 0.4)
    }
}
