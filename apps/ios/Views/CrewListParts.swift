import KithKit
import KithUI
import SwiftUI

/// Makes a row draggable (and shows it lifted) only where the order setting allows.
struct LiftIfReorderable: ViewModifier {
    let reorderable: Bool
    let provider: () -> NSItemProvider

    func body(content: Content) -> some View {
        if reorderable { content.itemProvider(provider) } else { content }
    }
}

/// The item a lifted room row carries (nothing): the system lets go of it when the drag or the
/// menu ends.
final class LiftItem: NSItemProvider {
    private let ended: @Sendable () -> Void

    init(ended: @escaping @Sendable () -> Void) {
        self.ended = ended
        super.init()
    }

    deinit { ended() }
}

/// The crew before there is one: on the sky, in the new type.
struct EmptyRoster: View {
    let canCreate: Bool
    let hasComputer: Bool
    let create: () -> Void
    let showComputers: () -> Void

    var body: some View {
        VStack(spacing: 14) {
            CharacterAvatar(portrait: "spare-cloud", size: 72)
            Text("No agents yet").kithText(.topBar).foregroundStyle(Palette.ink)
            Text(hasComputer
                 ? "Make an agent for each kind of work: a reviewer, a fixer, a docs writer. Point it at a project."
                 : "Ask one of your account's computers for access, or pair one with its code.")
                .kithText(.body)
                .foregroundStyle(Palette.muted)
                .multilineTextAlignment(.center)
            if canCreate {
                Button("Make your first agent", action: create)
                    .buttonStyle(.primary)
            } else if !hasComputer {
                Button("Computers", action: showComputers)
                    .buttonStyle(.primary)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 32)
        .padding(.vertical, 40)
    }
}
