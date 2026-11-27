import KithKit
import SwiftUI

/// A face for an agent: its own chat's avatar, or a letter when the app doesn't know the agent.
struct FeedFace: View {
    let bot: Bot?
    let agent: String
    let size: CGFloat

    var body: some View {
        if let bot {
            BotAvatar(bot: bot, size: size)
        } else {
            Text(String(agent.prefix(1)).uppercased())
                .font(KithText.topBar.font)
                .foregroundStyle(Palette.accent)
                .frame(width: size, height: size)
                .background(Palette.accentDim, in: Circle())
        }
    }
}

/// One post, flat like a social post: the face in a column on the left, name and time on one line, the title and
/// one sentence, one attachment, then heart and reply. Tapping the post opens its chat; long-press has the rest.
struct FeedCard: View {
    let post: HostContract.Post
    let feed: FeedStore
    let onOpenChat: (_ chat: String, _ entry: String?) -> Void
    @State private var replying = false
    @State private var heart = 0

    private var agentBot: Bot? { feed.agentBot(post.agent) }
    private var agentName: String { agentBot?.name ?? post.agent }
    private var removed: Bool { feed.isChatRemoved(post) }

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            FeedFace(bot: agentBot, agent: post.agent, size: 40)
            VStack(alignment: .leading, spacing: 8) {
                header
                VStack(alignment: .leading, spacing: 3) {
                    Text(post.title).font(KithText.topBar.font).foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
                    if !post.line.isEmpty {
                        Text(post.line).font(KithText.body.font).foregroundStyle(Palette.ink2).fixedSize(horizontal: false, vertical: true)
                    }
                }
                FeedAttachmentView(post: post, feed: feed)
                FeedFailingRow(post: post, removed: removed, onOpen: open)
                actions
                if replying { FeedReplyBar(post: post, feed: feed, onClose: { replying = false }) }
            }
        }
        .padding(.horizontal, KithSpacing.gutter)
        .padding(.vertical, 12)
        .contentShape(Rectangle())
        .onTapGesture(perform: open)
        .contextMenu {
            Button("Open in chat", systemImage: "bubble.left", action: open).disabled(removed)
            Button("Hide", systemImage: "eye.slash", role: .destructive) { Task { await feed.hide(post.id) } }
        }
        .overlay(alignment: .bottom) { Rectangle().fill(Palette.line).frame(height: 0.5) }
        .accessibilityElement(children: .contain)
    }

    private func open() {
        if !removed { onOpenChat(post.source.chat, post.source.entry) }
    }

    private var header: some View {
        HStack(spacing: 6) {
            Text(agentName).font(KithText.secondary.font).fontWeight(.semibold).foregroundStyle(Palette.ink)
            Text(RelativeTime.short(Date(timeIntervalSince1970: Double(post.updatedAt) / 1000)))
                .font(KithText.secondary.font).foregroundStyle(Palette.muted)
            Spacer(minLength: 0)
            if post.kind == "yourTurn" {
                HStack(spacing: 5) {
                    Circle().fill(Palette.attention).frame(width: 7, height: 7)
                    Text("Your turn").font(KithText.label.font).foregroundStyle(Palette.attention)
                }
            }
        }
    }

    private var actions: some View {
        HStack(spacing: 22) {
            Button {
                heart += 1
                Task { await feed.toggleLike(post.id) }
            } label: {
                Image(systemName: post.liked ? "heart.fill" : "heart")
                    .foregroundStyle(post.liked ? Palette.negative : Palette.muted)
                    .symbolEffect(.bounce, value: heart)
                    .frame(minWidth: 36, minHeight: 32, alignment: .leading)
            }
            .accessibilityLabel(post.liked ? "Unlike" : "Like")
            Button {
                withAnimation(Motion.layout) { replying.toggle() }
            } label: {
                Image(systemName: "bubble.left").foregroundStyle(replying ? Palette.accent : Palette.muted).frame(minWidth: 36, minHeight: 32, alignment: .leading)
            }
            .accessibilityLabel("Reply")
            Spacer(minLength: 0)
            if removed { Text("chat removed").font(KithText.label.font).foregroundStyle(Palette.muted) }
        }
        .buttonStyle(.plain)
    }
}

/// A routine that keeps failing: one quiet grey row, no alarm colours.
struct FeedFailingRow: View {
    let post: HostContract.Post
    let removed: Bool
    let onOpen: () -> Void

    var body: some View {
        if post.kind == "failing", post.streak > 0 {
            HStack(spacing: 6) {
                Text(text).font(KithText.secondary.font).foregroundStyle(Palette.muted)
                Spacer(minLength: 0)
                if !removed { Button("Open ›", action: onOpen).font(KithText.secondary.font).foregroundStyle(Palette.accent) }
            }
            .padding(.horizontal, 10).padding(.vertical, 8)
            .background(Palette.panel, in: RoundedRectangle(cornerRadius: KithRadius.rowTool, style: .continuous))
        }
    }

    /// "3 times since Tue": the oldest of the failed runs in the streak.
    private var text: String {
        let failed = post.items.compactMap { item -> Int64? in
            guard case let .object(o) = item, case let .int(at)? = o["at"], case let .bool(ok)? = o["ok"], !ok else { return nil }
            return at
        }.sorted(by: >).prefix(Int(post.streak))
        let times = post.streak == 1 ? "Failed once" : "\(post.streak) times"
        guard let first = failed.last else { return times + " in a row" }
        let day = Date(timeIntervalSince1970: Double(first) / 1000).formatted(.dateTime.weekday(.abbreviated))
        return "\(times) since \(day) · this post updates"
    }
}
