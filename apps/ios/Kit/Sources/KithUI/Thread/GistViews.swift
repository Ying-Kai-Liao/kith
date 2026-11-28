import KithKit
import SwiftUI

/// A long reply folded to its first sentence, with a quiet "See full" under it. Tapping opens this one reply.
struct GistBubble: View {
    let gist: String
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            VStack(alignment: .leading, spacing: 6) {
                // A very long first sentence stops at two lines.
                Text(gist)
                    .kithText(.body)
                    .foregroundStyle(Palette.ink)
                    .lineLimit(2)
                    .truncationMode(.tail)
                    .multilineTextAlignment(.leading)
                Text("See full").kithText(.secondary).foregroundStyle(Palette.muted)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: KithRadius.card + 4, style: .continuous))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Summary: \(gist)")
        .accessibilityHint("Folded. Opens the full reply")
    }
}

/// "See less" under an opened long reply while the chat is in Gist mode.
struct GistToggle: View {
    let title: String
    let hint: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title).kithText(.secondary).foregroundStyle(Palette.muted)
                .frame(minHeight: 28, alignment: .leading)
        }
        .buttonStyle(.plain)
        .accessibilityHint(hint)
    }
}

/// A run someone else prompted, folded to one line: "Worked with coder · 6 messages". Tapping opens it in place.
struct OthersRunLine: View {
    let reason: FoldReason
    let expanded: Bool
    let toggle: () -> Void

    var body: some View {
        if case let .othersRun(title, count, live) = reason {
            Button(action: toggle) {
                HStack(spacing: 10) {
                    Rectangle().fill(Palette.border).frame(height: 1)
                    HStack(spacing: 6) {
                        if live { Spinner(size: 12) }
                        Text("\(title) · \(OthersRun.countText(count))")
                            .kithText(.secondary)
                            .foregroundStyle(Palette.muted)
                            .lineLimit(1)
                            .truncationMode(.tail)
                        Image(systemName: "chevron.down")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(Palette.muted)
                            .rotationEffect(.degrees(expanded ? 180 : 0))
                    }
                    .layoutPriority(1)
                    Rectangle().fill(Palette.border).frame(height: 1)
                }
                .frame(minHeight: 44)
                .contentShape(Rectangle())
            }
            .buttonStyle(PressScale())
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(title), \(OthersRun.countText(count))\(live ? ", still going" : "")")
            .accessibilityValue(expanded ? "Opened" : "Folded")
            .accessibilityHint(expanded ? "Hides these messages" : "Opens these messages")
        }
    }
}
