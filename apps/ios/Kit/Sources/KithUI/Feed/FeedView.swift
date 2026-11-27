import KithKit
import SwiftUI

/// The owner's Feed: rings on top, a card per post, the bottom box. The one entry the app's slot hosts; it makes
/// no navigation of its own: "open in chat" and a ring go through `onOpenChat`.
public struct FeedView: View {
    let feed: FeedStore
    let composer: FeedComposer
    let onOpenChat: (_ chat: String, _ entry: String?) -> Void

    public init(feed: FeedStore, composer: FeedComposer, onOpenChat: @escaping (_ chat: String, _ entry: String?) -> Void) {
        self.feed = feed
        self.composer = composer
        self.onOpenChat = onOpenChat
    }

    public init(store: BotStore, onOpenChat: @escaping (_ chat: String, _ entry: String?) -> Void) {
        self.init(feed: store.feed, composer: store.feedComposer, onOpenChat: onOpenChat)
    }

    public var body: some View {
        content
            .background(Palette.background)
            .safeAreaInset(edge: .bottom) {
                if feed.phase != .needsUpdate { FeedComposerBar(composer: composer, onOpenChat: onOpenChat) }
            }
            .task { await feed.load() }
            .alert("Feed", isPresented: Binding(get: { feed.notice != nil }, set: { if !$0 { feed.notice = nil } })) {
                Button("OK") { feed.notice = nil }
            } message: { Text(feed.notice ?? "") }
    }

    @ViewBuilder
    private var content: some View {
        switch feed.phase {
        case .needsUpdate:
            note("Update Kith on your Mac to see the Feed.", systemImage: "arrow.down.circle")
        case .offline(let why) where feed.posts.isEmpty:
            note(why, systemImage: "wifi.slash")
        case .idle, .loading:
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        default:
            list
        }
    }

    private var list: some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                FeedStoriesRow(feed: feed, onOpenChat: onOpenChat).padding(.bottom, 10)
                if feed.posts.isEmpty {
                    note("Nothing new. Posts from your agents show up here.", systemImage: "sparkles")
                        .frame(minHeight: 280)
                }
                ForEach(feed.posts, id: \.id) { post in
                    FeedCard(post: post, feed: feed, onOpenChat: onOpenChat)
                        .task { if post.id == feed.posts.last?.id { await feed.loadMore() } }
                }
            }
            .padding(.vertical, 8)
        }
        .refreshable { await feed.load() }
        .scrollDismissesKeyboard(.interactively)
    }

    private func note(_ text: String, systemImage: String) -> some View {
        VStack(spacing: 10) {
            Image(systemName: systemImage).font(.system(size: 28)).foregroundStyle(Palette.faint)
            Text(text).font(KithText.body.font).foregroundStyle(Palette.muted).multilineTextAlignment(.center)
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
