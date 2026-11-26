import KithKit
import KithUI
import SwiftUI

/// The Feed place until the feed is built. The Feed package replaces this view and nothing around it.
struct FeedPlaceholderView: View {
    var body: some View {
        EmptyState(title: "Feed is coming", systemImage: "square.stack",
                   message: "What your agents and people did, in one place.")
            .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
