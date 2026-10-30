import KithKit
import SwiftUI
import UIKit

/// One picture of a message, for the grid and the swipeable viewer. `load` gives the bytes (nil when they can't be had).
struct GalleryImage: Identifiable {
    let id: String
    let name: String
    let load: () async -> Data?
}

/// Which tiles the grid draws: at most `cap`, the last one carrying "+N" when more follow.
enum ImageGridPlan {
    static let cap = 4

    static func visible(_ count: Int) -> Int { min(count, cap) }
    static func overflow(_ count: Int) -> Int { max(0, count - cap) }

    /// A picture the chat draws inline (and so can join a grid): an image name, a size worth loading, small enough to hold.
    static func isInline(_ file: SharedFile) -> Bool {
        Attachment(id: file.id, name: file.name, size: file.size).isImage && file.size > 0 && file.size <= SharedFileView.maxInlineBytes
    }

    /// The pictures of an agent entry's files, and the rest (shown as download cards).
    static func split(_ files: [SharedFile]) -> (images: [SharedFile], others: [SharedFile]) {
        (files.filter(isInline), files.filter { !isInline($0) })
    }
}

/// Two or more pictures as a compact grid; a tap opens the viewer on that picture.
struct ImageGrid: View {
    let images: [GalleryImage]
    var trailing = false
    let fill: Color
    @State private var opened: Int?

    private static let tile: CGFloat = 112
    private static let gap: CGFloat = 4

    var body: some View {
        let shown = ImageGridPlan.visible(images.count)
        let more = ImageGridPlan.overflow(images.count)
        let columns = [GridItem(.fixed(Self.tile), spacing: Self.gap), GridItem(.fixed(Self.tile), spacing: Self.gap)]
        LazyVGrid(columns: columns, alignment: trailing ? .trailing : .leading, spacing: Self.gap) {
            ForEach(0..<shown, id: \.self) { index in
                let overflowing = more > 0 && index == shown - 1
                ImageTile(image: images[index], fill: fill, extra: overflowing ? more + 1 : 0)
                    .frame(width: Self.tile, height: Self.tile)
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .contentShape(Rectangle())
                    .onTapGesture { opened = index }
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(overflowing ? "\(more + 1) more images" : images[index].name)
                    .accessibilityAddTraits(.isButton)
            }
        }
        .frame(width: Self.tile * 2 + Self.gap)
        .kithSheet(item: Binding(get: { opened.map(OpenedImage.init) }, set: { opened = $0?.index })) { open in
            ImageGalleryViewer(images: images, start: open.index)
        }
    }

    private struct OpenedImage: Identifiable {
        let index: Int
        var id: Int { index }
    }
}

private struct ImageTile: View {
    let image: GalleryImage
    let fill: Color
    /// More than 0: the tile that stands for this many pictures, itself included.
    let extra: Int
    @State private var thumbnail: CGImage?
    @State private var failed = false

    var body: some View {
        ZStack {
            fill
            if let thumbnail {
                Image(decorative: thumbnail, scale: 1).resizable().aspectRatio(contentMode: .fill)
            } else {
                // Still loading, or unreadable: the tile stays, and the viewer shows what it can.
                Image(systemName: failed ? "exclamationmark.triangle" : "photo").foregroundStyle(Palette.tertiary)
            }
            if extra > 0 {
                Color.black.opacity(0.45)
                Text("+\(extra)").font(.title2.weight(.semibold)).foregroundStyle(.white)
            }
        }
        .task(id: image.id) {
            guard let bytes = await image.load() else { failed = true; return }
            guard let small = await Task.detached(operation: { ImageData.thumbnail(bytes, maxPixels: 360) }).value else {
                failed = true
                return
            }
            thumbnail = small
        }
    }
}

/// Every picture of a message, swiped between at full size. Share works on the current one from a copy under its own name.
struct ImageGalleryViewer: View {
    let images: [GalleryImage]
    @State private var current: String
    @State private var copies: [String: URL] = [:]

    init(images: [GalleryImage], start: Int) {
        self.images = images
        _current = State(initialValue: images.indices.contains(start) ? images[start].id : images.first?.id ?? "")
    }

    var body: some View {
        let name = images.first { $0.id == current }?.name ?? ""
        VStack(spacing: 0) {
            ModalHeader(name) {
                if let url = copies[current] {
                    ShareLink(item: url) {
                        Image(systemName: "square.and.arrow.up").font(.system(size: 17)).frame(width: 44, height: 44)
                    }
                    .accessibilityLabel("Share \(name)")
                }
            }
            TabView(selection: $current) {
                ForEach(images) { image in
                    GalleryPage(image: image) { copies[image.id] = $0 }
                        .tag(image.id)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
        }
        .background(Palette.background)
        .onDisappear {
            for url in copies.values { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        }
    }
}

private struct GalleryPage: View {
    let image: GalleryImage
    let copied: (URL) -> Void
    @State private var picture: CGImage?
    @State private var failed = false

    var body: some View {
        Group {
            if let picture {
                Image(decorative: picture, scale: 1).resizable().aspectRatio(contentMode: .fit)
            } else if failed {
                Text("Can't show this image").font(.subheadline).foregroundStyle(Palette.secondary)
            } else {
                ProgressView()
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(12)
        .task(id: image.id) {
            guard let data = await image.load() else { failed = true; return }
            guard let decoded = await Task.detached(operation: { ImageData.thumbnail(data, maxPixels: 4096) }).value else {
                failed = true
                return
            }
            picture = decoded
            if let url = Self.copy(data, named: image.name) { copied(url) }
        }
    }

    private static func copy(_ data: Data, named name: String) -> URL? {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("KithShare-\(UUID().uuidString)", isDirectory: true)
        // A name with a path in it would write outside the folder, or fail and leave the viewer without Share.
        let leaf = (name as NSString).lastPathComponent
        let url = folder.appendingPathComponent(leaf.isEmpty ? "image" : leaf)
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            try data.write(to: url)
            return url
        } catch { return nil }
    }
}
