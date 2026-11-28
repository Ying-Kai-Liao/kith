import KithKit
import KithUI
import SwiftUI

/// The top bar of both places, as the mock's tabTop(): your picture (the profile menu) on the left, the Crew | Feed
/// switch in the middle, and whatever the place needs on the right.
struct HomeTopBar<Trailing: View>: View {
    @Binding var tab: HomeTab
    @ViewBuilder let trailing: Trailing

    var body: some View {
        HStack {
            ProfileMenu()
            Spacer(minLength: 8)
            HomeTabSwitch(selection: $tab)
            Spacer(minLength: 8)
            trailing
        }
        .padding(.horizontal, KithSpacing.gutter)
        .padding(.top, 4)
    }
}

/// The Feed place: its own navigation stack under the shared sky and top bar, showing the owner's Feed.
struct HomeFeedStack: View {
    @Binding var tab: HomeTab
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts

    /// The feed is the owner's: the first computer you own.
    private var feedStore: BotStore? {
        accounts.computers.lazy.compactMap { accounts.store(for: $0.id) }.first { $0.isOwner }
    }

    private static var isDemo: Bool {
        #if DEBUG
        DemoScreens.name == "feed"
        #else
        false
        #endif
    }

    /// Opens a chat the way Crew does: through `AppStore.openBot`, landing on the entry when given.
    private func openChat(_ store: BotStore, chat: String, entry: String?) {
        store.focusEntry = entry.map { .init(chat: chat, entry: $0) }
        app.openBot(BotReference(accountId: accounts.accountId, computerId: store.computer.id, botId: chat))
    }

    var body: some View {
        NavigationStack {
            ZStack(alignment: .top) {
                SkyBackground(.home)
                if Self.isDemo {
                    #if DEBUG
                    FeedDemoScreen()
                    #endif
                } else if let store = feedStore {
                    FeedView(store: store) { chat, entry in openChat(store, chat: chat, entry: entry) }
                        .id(store.computer.id)
                } else {
                    Text("Connect a computer to see the Feed.").foregroundStyle(Palette.secondary)
                }
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                // The same width as Crew's new-chat button, so the switch sits where it does there.
                HomeTopBar(tab: $tab) { Color.clear.frame(width: 44, height: 44) }
            }
            .toolbar(.hidden, for: .navigationBar)
        }
        .tint(Palette.text)
    }
}

#if DEBUG
/// `-kithDemoScreens feed`: the Feed over invented posts.
private struct FeedDemoScreen: View {
    @State private var parts = FeedDemo.make()
    var body: some View { FeedView(feed: parts.feed, composer: parts.composer) { _, _ in } }
}
#endif
