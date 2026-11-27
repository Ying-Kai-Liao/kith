import KithKit
import SwiftUI

/// The one attachment of a post. A kind or an attachment type this app doesn't know shows nothing extra: the
/// card is the plain card.
struct FeedAttachmentView: View {
    let post: HostContract.Post
    let feed: FeedStore
    @State private var showAllMerges = false

    var body: some View {
        if post.kind == "merges", !post.merges.isEmpty {
            merges
        } else {
            switch post.feedAttachment {
            case let .number(value, unit, delta): number(value, unit, delta)
            case let .page(file, shot, _): page(file: file, shot: shot)
            case let .file(file): row("doc", file.name, Self.size(file.size))
            case let .video(file): row("play.rectangle", file.name, Self.size(file.size))
            case let .link(url): row("link", url.host ?? url.absoluteString, nil)
            case .unknown, nil: EmptyView()
            }
        }
    }

    // Small items fold: three PRs show, the rest sit behind one line.
    private var merges: some View {
        let all = post.merges
        let shown = showAllMerges ? all : Array(all.prefix(3))
        return VStack(alignment: .leading, spacing: 6) {
            ForEach(shown) { m in
                HStack(spacing: 8) {
                    Text(m.id).font(KithText.code.font).foregroundStyle(Palette.muted).lineLimit(1)
                    Text(m.title).font(KithText.secondary.font).foregroundStyle(Palette.ink2).lineLimit(1)
                }
            }
            if all.count > shown.count {
                Button("+\(all.count - shown.count) more") { withAnimation(Motion.layout) { showAllMerges = true } }
                    .font(KithText.secondary.font).foregroundStyle(Palette.accent)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func number(_ value: Double, _ unit: String?, _ delta: Double?) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(Self.format(value)).font(.system(size: 28, weight: .bold, design: .rounded)).foregroundStyle(Palette.ink).monospacedDigit()
            if let unit { Text(unit).font(KithText.body.font).foregroundStyle(Palette.muted) }
            if let delta, delta != 0 {
                Label(Self.format(abs(delta)), systemImage: delta > 0 ? "arrow.up.right" : "arrow.down.right")
                    .font(KithText.secondary.font)
                    .foregroundStyle(delta > 0 ? Palette.positive : Palette.negative)
            }
        }
    }

    @ViewBuilder
    private func page(file: SharedFile?, shot: SharedFile?) -> some View {
        ZStack {
            if let shot {
                FeedShotImage(post: post, shot: shot, feed: feed) { titleCard(file?.name) }
            } else {
                titleCard(file?.name)
            }
        }
        .frame(height: 120)
        .frame(maxWidth: .infinity)
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Palette.line))
    }

    /// No picture yet (or none will come): the title on the accent, so a page still reads as a page.
    private func titleCard(_ name: String?) -> some View {
        ZStack(alignment: .bottomLeading) {
            LinearGradient(colors: [Palette.accent.opacity(0.22), Palette.accent.opacity(0.10)], startPoint: .topLeading, endPoint: .bottomTrailing)
            VStack(alignment: .leading, spacing: 2) {
                Image(systemName: "doc.richtext").font(.title3)
                // The post already says its title; the file name only when it adds something.
                if let name, name != post.title { Text(name).font(KithText.label.font).opacity(0.8) }
            }
            .foregroundStyle(Palette.accent)
            .padding(14)
        }
    }

    private func row(_ icon: String, _ title: String, _ detail: String?) -> some View {
        HStack(spacing: 10) {
            Image(systemName: icon).foregroundStyle(Palette.accent).frame(width: 28)
            VStack(alignment: .leading, spacing: 0) {
                Text(title).font(KithText.secondary.font).foregroundStyle(Palette.ink).lineLimit(1)
                if let detail { Text(detail).font(KithText.label.font).foregroundStyle(Palette.muted) }
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .background(Palette.background, in: RoundedRectangle(cornerRadius: KithRadius.rowTool, style: .continuous))
    }

    static func size(_ bytes: Int64) -> String { ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file) }

    static func format(_ v: Double) -> String {
        v == v.rounded() && abs(v) < 1e15 ? String(Int64(v)) : v.formatted(.number.precision(.fractionLength(0...2)))
    }
}

/// A page's picture, read through its source entry once it is on screen.
private struct FeedShotImage<Fallback: View>: View {
    let post: HostContract.Post
    let shot: SharedFile
    let feed: FeedStore
    @ViewBuilder var fallback: () -> Fallback
    @State private var image: UIImage?

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                fallback()
            }
        }
        .task(id: shot.id) {
            if let data = await feed.shot(for: post, file: shot) { image = UIImage(data: data) }
        }
    }
}
