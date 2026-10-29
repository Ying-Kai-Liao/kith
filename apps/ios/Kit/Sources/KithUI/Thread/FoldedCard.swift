import KithKit
import SwiftUI

/// An answered or finished card as one pill line: a green check or red x, what happened, the time. Tapping opens the
/// read-only card (`full`); tapping the pill line again closes it.
struct FoldedCard<Full: View>: View {
    let fold: CardFold
    let time: String
    @ViewBuilder let full: () -> Full
    @State private var open = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { open.toggle() }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: fold.ok ? "checkmark.circle.fill" : "xmark.circle.fill")
                        .foregroundStyle(fold.ok ? Palette.positive : Palette.negative)
                    Text(fold.text)
                        .kithText(.secondary)
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 8)
                    Text(time).kithText(.code).foregroundStyle(Palette.muted)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Palette.panel, in: Capsule())
                .contentShape(Capsule())
            }
            .buttonStyle(PressScale())
            .accessibilityLabel("\(fold.ok ? "Done" : "Not done"): \(fold.text), \(time)")
            .accessibilityHint(open ? "Hides the card" : "Shows the card")
            if open { full() }
        }
    }
}
