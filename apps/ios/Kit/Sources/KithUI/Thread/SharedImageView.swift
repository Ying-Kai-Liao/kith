import KithKit
import SwiftUI
import UIKit

/// A file an agent shared: a picture shows as a picture (tap for full size, with Share and Save Image), anything
/// else, or a picture too large or unreadable to show, as the download card.
struct SharedFileView: View {
    let entry: Entry
    let file: SharedFile
    /// Past this a picture stays a card: it would take long to load and hold a lot of memory.
    static let maxInlineBytes: Int64 = 25_000_000

    var body: some View {
        if ImageGridPlan.isInline(file) {
            SharedImage(entry: entry, file: file)
        } else {
            FileDownloadCard(entry: entry, file: file)
        }
    }
}

private struct SharedImage: View {
    let entry: Entry
    let file: SharedFile
    @Environment(BotStore.self) private var model
    @State private var thumbnail: CGImage?
    @State private var data: Data?
    @State private var failed = false
    @State private var viewing = false

    var body: some View {
        if failed {
            FileDownloadCard(entry: entry, file: file)
        } else {
            Group {
                if let thumbnail {
                    Image(decorative: thumbnail, scale: 1).resizable().aspectRatio(contentMode: .fit)
                        .frame(maxWidth: 240, maxHeight: 300)
                        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
                        .contentShape(Rectangle())
                        .onTapGesture { viewing = true }
                        .accessibilityLabel(file.name)
                        .accessibilityAddTraits(.isButton)
                } else {
                    RoundedRectangle(cornerRadius: 16, style: .continuous)
                        .fill(Palette.bubbleAgent)
                        .frame(width: 160, height: 120)
                        .overlay { Image(systemName: "photo").foregroundStyle(Palette.tertiary) }
                }
            }
            .task(id: file.id) {
                guard let bytes = await model.fileDownloads.bytes(client: model.client, entry: entry, file: file) else {
                    failed = true
                    return
                }
                // The picture is decoded small for the chat; a format this phone can't draw falls back to the card.
                guard let small = await Task.detached(operation: { ImageData.thumbnail(bytes, maxPixels: 720) }).value else {
                    failed = true
                    return
                }
                data = bytes
                thumbnail = small
            }
            .kithSheet(isPresented: $viewing) {
                SharedImageViewer(file: file, data: data)
            }
        }
    }
}

/// The picture at full size. Share (and Save Image in the share sheet) works from a copy under its own name.
private struct SharedImageViewer: View {
    let file: SharedFile
    let data: Data?
    @State private var image: CGImage?
    @State private var shared: URL?

    var body: some View {
        VStack(spacing: 0) {
            ModalHeader(file.name) {
                if let shared {
                    ShareLink(item: shared) {
                        Image(systemName: "square.and.arrow.up").font(.system(size: 17)).frame(width: 44, height: 44)
                    }
                    .accessibilityLabel("Share \(file.name)")
                }
            }
            Group {
                if let image {
                    Image(decorative: image, scale: 1).resizable().aspectRatio(contentMode: .fit)
                } else {
                    ProgressView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(12)
        }
        .background(Palette.background)
        .task {
            guard let data else { return }
            image = await Task.detached(operation: { ImageData.thumbnail(data, maxPixels: 4096) }).value
            shared = Self.copy(data, named: file.name)
        }
        .onDisappear { if let shared { try? FileManager.default.removeItem(at: shared.deletingLastPathComponent()) } }
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
