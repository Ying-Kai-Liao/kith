import KithKit
import KithUI
import SwiftUI

/// Cold launch: the Kith cube bot fades and scales in over the system launch screen while the
/// computers connect and catch up, then zooms away onto a list that's already current.
/// Held at least `shortest` so the motion reads, never past `longest` (the cached list is behind it).
struct LaunchSplash: View {
    let stores: [BotStore]
    let done: () -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var shown = false
    @State private var waited = false

    private static let shortest: Duration = .milliseconds(700)
    private static let longest: Duration = .milliseconds(1800)

    private var ready: Bool { waited && stores.allSatisfy(\.caughtUp) }

    var body: some View {
        Image("KithMark")
            .resizable()
            .scaledToFit()
            .frame(width: 128, height: 128)
        .scaleEffect(shown || reduceMotion ? 1 : 0.85)
        .opacity(shown ? 1 : 0)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background { SkyBackground() }
        .accessibilityLabel("Connecting")
        .task {
            withAnimation(.easeOut(duration: 0.4)) { shown = true }
            try? await Task.sleep(for: Self.shortest)
            waited = true
            try? await Task.sleep(for: Self.longest - Self.shortest)
            done()
        }
        .onChange(of: ready, initial: true) { _, ready in if ready { done() } }
        // Leaving: the mark grows a little as everything fades through to the app.
        .transition(reduceMotion ? .opacity : .opacity.combined(with: .scale(scale: 1.15)))
    }
}
