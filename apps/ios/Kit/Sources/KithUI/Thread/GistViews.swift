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
