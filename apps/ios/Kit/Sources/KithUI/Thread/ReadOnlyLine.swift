import KithKit
import SwiftUI

/// Where the composer would be in a chat the viewer only reads (a bot from outside asks there): one quiet line.
struct ReadOnlyLine: View {
    let text: String

    var body: some View {
        Text(text)
            .kithText(.secondary)
            .foregroundStyle(Palette.muted)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 24)
            .padding(.vertical, 14)
            .accessibilityLabel(Text(text))
    }
}
