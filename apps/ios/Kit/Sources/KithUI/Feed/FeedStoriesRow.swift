import KithKit
import SwiftUI

/// Rings for the chats where an agent spoke in the last three hours. Violet when there is something unread.
/// It re-reads the clock every minute so a ring ends on time without a refetch.
struct FeedStoriesRow: View {
    let feed: FeedStore
    let onOpenChat: (_ chat: String, _ entry: String?) -> Void

    var body: some View {
        TimelineView(.everyMinute) { context in
            let live = feed.liveStories(now: context.date)
            if !live.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 14) {
                        ForEach(live, id: \.chat) { story in
                            Button { onOpenChat(story.chat, story.lastEntryId) } label: { ring(story) }
                                .buttonStyle(.plain)
                        }
                    }
                    .padding(.horizontal, KithSpacing.gutter)
                }
            }
        }
    }

    private func ring(_ story: HostContract.Story) -> some View {
        let bot = feed.chat(story.chat)
        let name = bot?.name ?? story.agent ?? "Chat"
        return VStack(spacing: 4) {
            FeedFace(bot: bot, agent: story.agent ?? name, size: 52)
                .padding(3)
                .overlay(Circle().strokeBorder(story.unread > 0 ? AnyShapeStyle(Palette.accent) : AnyShapeStyle(Palette.line), lineWidth: story.unread > 0 ? 2.5 : 1.5))
            Text(name).font(KithText.label.font).foregroundStyle(Palette.ink2).lineLimit(1).frame(width: 64)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(story.unread > 0 ? "\(name), \(story.unread) unread" : name)
    }
}
