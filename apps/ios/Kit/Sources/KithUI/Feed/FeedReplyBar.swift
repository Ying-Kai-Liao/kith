import KithKit
import SwiftUI

/// The inline reply under a card: goes to the post's chat and thread. @ chips name the group's agents only;
/// a removed chat has no box.
struct FeedReplyBar: View {
    let post: HostContract.Post
    let feed: FeedStore
    let onClose: () -> Void
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        if feed.isChatRemoved(post) {
            Text("chat removed").font(KithText.secondary.font).foregroundStyle(Palette.muted)
        } else {
            VStack(alignment: .leading, spacing: 8) {
                let members = feed.mentionable(in: post)
                if !members.isEmpty {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 6) {
                            ForEach(members) { bot in
                                Button("@\(bot.name)") { text += (text.isEmpty || text.hasSuffix(" ") ? "" : " ") + "@\(bot.name) " }
                                    .font(KithText.label.font)
                                    .padding(.horizontal, 10).padding(.vertical, 5)
                                    .background(Palette.accentDim, in: Capsule())
                                    .foregroundStyle(Palette.accent)
                            }
                        }
                    }
                }
                HStack(alignment: .bottom, spacing: 8) {
                    TextField("Reply", text: $text, axis: .vertical)
                        .lineLimit(1...4)
                        .font(KithText.body.font)
                        .focused($focused)
                        .padding(.horizontal, 12).padding(.vertical, 8)
                        .background(Palette.background, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
                    Button {
                        Task { if await feed.reply(to: post, text: text) { text = ""; onClose() } }
                    } label: {
                        Image(systemName: "arrow.up.circle.fill").font(.system(size: 30))
                            .foregroundStyle(canSend ? Palette.accentFill : Palette.faint)
                    }
                    .disabled(!canSend)
                    .accessibilityLabel("Send reply")
                }
            }
            .onAppear { focused = true }
        }
    }

    private var canSend: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !feed.replying.contains(post.id)
    }
}
