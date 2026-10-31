import CryptoKit
import Foundation
import ImageIO

/// What the app got for one news link: the page's picture (big enough to show) and favicon, as image bytes.
public struct LinkPreviewImages: Equatable, Sendable {
    public var picture: Data?
    public var icon: Data?
    public static let none = LinkPreviewImages(picture: nil, icon: nil)
}

/// The two requests a preview takes. The real one is `LinkPreviewSession`; tests swap in a fake.
public protocol LinkPreviewLoading: Sendable {
    /// The start of the page's HTML (at most `maxPageBytes`), or nil.
    func page(_ url: URL) async -> String?
    /// Image bytes (at most `maxImageBytes`, `image/*` only), or nil.
    func image(_ url: URL) async -> Data?
}

/// Fetches a news link's picture and favicon once, remembers the answer in memory and on disk (misses too, for a week)
/// and never asks the web when the Privacy switch is off. One request per link at a time, four links at once.
public actor LinkPreviewStore {
    public static let shared = LinkPreviewStore(
        directory: FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("LinkPreviews"),
        loader: LinkPreviewSession())

    static let life: TimeInterval = 7 * 24 * 3600
    static let diskBudget = 40 * 1024 * 1024
    static let parallel = 4

    private let directory: URL
    private let loader: any LinkPreviewLoading
    private let enabled: @Sendable () -> Bool
    private let now: @Sendable () -> Date
    private let memory = NSCache<NSString, Box>()
    private var inFlight: [URL: Task<LinkPreviewImages, Never>] = [:]
    private var running = 0
    private var waiting: [CheckedContinuation<Void, Never>] = []

    private final class Box { let images: LinkPreviewImages; init(_ i: LinkPreviewImages) { images = i } }
    private struct Meta: Codable { var at: Date; var picture: Bool; var icon: Bool }

    public init(directory: URL, loader: any LinkPreviewLoading, enabled: @escaping @Sendable () -> Bool = { LinkPreview.isEnabled },
                now: @escaping @Sendable () -> Date = { .now }) {
        self.directory = directory
        self.loader = loader
        self.enabled = enabled
        self.now = now
        memory.countLimit = 200
    }

    /// The cached answer, else one fetch (shared by every caller asking for the same link). Nil with the switch off.
    public func images(for page: URL) async -> LinkPreviewImages? {
        guard enabled(), LinkPreview.isFetchable(page) else { return nil }
        let key = page.absoluteString as NSString
        if let hit = memory.object(forKey: key) { return hit.images }
        if let disk = readDisk(page) { memory.setObject(Box(disk), forKey: key); return disk }
        let task: Task<LinkPreviewImages, Never>
        if let existing = inFlight[page] { task = existing } else {
            task = Task { await self.fetch(page) }
            inFlight[page] = task
        }
        let result = await task.value
        // Turned off while it loaded: show nothing.
        return enabled() ? result : nil
    }

    /// Starts every link's fetch without waiting, so rows behind "Show N more" are warm when they open.
    public nonisolated func warm(_ pages: [URL]) {
        for p in pages { Task { _ = await self.images(for: p) } }
    }

    private func fetch(_ page: URL) async -> LinkPreviewImages {
        await acquire()
        let result = enabled() ? await load(page) : .none
        release()
        inFlight[page] = nil
        guard enabled() else { return .none }
        memory.setObject(Box(result), forKey: page.absoluteString as NSString)
        writeDisk(page, result)
        return result
    }

    private func load(_ page: URL) async -> LinkPreviewImages {
        guard let html = await loader.page(page) else { return .none }
        let head = LinkPreview.parse(html: html, base: page)
        async let pictureData: Data? = head.image == nil ? nil : loader.image(head.image!)
        async let iconData: Data? = head.icon == nil ? nil : loader.image(head.icon!)
        let (p, i) = await (pictureData, iconData)
        let picture = p.flatMap { Self.shortSide(of: $0) >= LinkPreview.minShortSide ? $0 : nil }
        let icon = i.flatMap { Self.shortSide(of: $0) > 0 ? $0 : nil }
        return LinkPreviewImages(picture: picture, icon: icon)
    }

    private func acquire() async {
        if running < Self.parallel { running += 1; return }
        await withCheckedContinuation { waiting.append($0) }
    }

    private func release() {
        if waiting.isEmpty { running -= 1 } else { waiting.removeFirst().resume() }
    }

    /// Pixel size of the shorter side, 0 when the bytes aren't an image.
    static func shortSide(of data: Data) -> Int {
        guard let src = CGImageSourceCreateWithData(data as CFData, nil),
              let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any],
              let w = props[kCGImagePropertyPixelWidth] as? Int, let h = props[kCGImagePropertyPixelHeight] as? Int else { return 0 }
        return min(w, h)
    }

    // MARK: Disk

    private func base(_ page: URL) -> URL {
        let hash = SHA256.hash(data: Data(page.absoluteString.utf8)).map { String(format: "%02x", $0) }.joined()
        return directory.appendingPathComponent(String(hash.prefix(40)))
    }

    private func readDisk(_ page: URL) -> LinkPreviewImages? {
        let b = base(page)
        guard let raw = try? Data(contentsOf: b.appendingPathExtension("json")),
              let meta = try? JSONDecoder().decode(Meta.self, from: raw),
              now().timeIntervalSince(meta.at) < Self.life else { return nil }
        let picture = meta.picture ? try? Data(contentsOf: b.appendingPathExtension("img")) : nil
        let icon = meta.icon ? try? Data(contentsOf: b.appendingPathExtension("ico")) : nil
        return LinkPreviewImages(picture: picture, icon: icon)
    }

    private func writeDisk(_ page: URL, _ images: LinkPreviewImages) {
        let b = base(page)
        let fm = FileManager.default
        try? fm.createDirectory(at: directory, withIntermediateDirectories: true)
        for (ext, data) in [("img", images.picture), ("ico", images.icon)] {
            if let data { try? data.write(to: b.appendingPathExtension(ext), options: .atomic) }
            else { try? fm.removeItem(at: b.appendingPathExtension(ext)) }
        }
        let meta = Meta(at: now(), picture: images.picture != nil, icon: images.icon != nil)
        if let raw = try? JSONEncoder().encode(meta) { try? raw.write(to: b.appendingPathExtension("json"), options: .atomic) }
        prune()
    }

    /// Oldest files go first once the folder passes its budget.
    private func prune() {
        let fm = FileManager.default
        let keys: [URLResourceKey] = [.contentModificationDateKey, .fileSizeKey]
        guard let files = try? fm.contentsOfDirectory(at: directory, includingPropertiesForKeys: keys) else { return }
        var sized = files.map { url -> (url: URL, date: Date, size: Int) in
            let v = try? url.resourceValues(forKeys: Set(keys))
            return (url, v?.contentModificationDate ?? .distantPast, v?.fileSize ?? 0)
        }
        var total = sized.reduce(0) { $0 + $1.size }
        guard total > Self.diskBudget else { return }
        sized.sort { $0.date < $1.date }
        for f in sized where total > Self.diskBudget * 3 / 4 {
            try? fm.removeItem(at: f.url)
            total -= f.size
        }
    }
}

/// The real loader: an ephemeral session (no cookies, no shared cache), 5 s timeouts, http(s) only, no private hosts
/// (redirects included), size caps, `image/*` only for pictures.
public final class LinkPreviewSession: LinkPreviewLoading, Sendable {
    private let session: URLSession

    public init() {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 5
        config.timeoutIntervalForResource = 10
        config.httpCookieStorage = nil
        config.httpShouldSetCookies = false
        config.urlCache = nil
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        session = URLSession(configuration: config, delegate: RedirectGuard(), delegateQueue: nil)
    }

    private final class RedirectGuard: NSObject, URLSessionTaskDelegate, Sendable {
        func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                        newRequest request: URLRequest) async -> URLRequest? {
            guard let url = request.url, await LinkPreview.resolvesPublic(url) else { return nil }
            return request
        }
    }

    public func page(_ url: URL) async -> String? {
        guard let (data, mime) = await read(url, limit: LinkPreview.maxPageBytes, accept: "text/html,application/xhtml+xml", cut: true),
              mime.isEmpty || mime.contains("html") else { return nil }
        return String(decoding: data, as: UTF8.self)
    }

    public func image(_ url: URL) async -> Data? {
        guard let (data, mime) = await read(url, limit: LinkPreview.maxImageBytes, accept: "image/*", cut: false),
              mime.hasPrefix("image/"), !mime.contains("svg") else { return nil }
        return data
    }

    /// Streams the body and stops at `limit` bytes: for a page the head is all that's wanted (`cut`), an oversized
    /// image is dropped.
    private func read(_ url: URL, limit: Int, accept: String, cut: Bool) async -> (Data, String)? {
        // Resolved here and on every redirect hop; a connection that re-resolves later is not pinned.
        guard await LinkPreview.resolvesPublic(url) else { return nil }
        var request = URLRequest(url: url)
        request.setValue(accept, forHTTPHeaderField: "Accept")
        request.setValue("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) Kith", forHTTPHeaderField: "User-Agent")
        guard let (bytes, response) = try? await session.bytes(for: request),
              let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else { return nil }
        let mime = (http.value(forHTTPHeaderField: "Content-Type") ?? "").lowercased()
        if !cut, http.expectedContentLength > Int64(limit) { return nil }
        var data = Data()
        do {
            for try await byte in bytes {
                data.append(byte)
                if data.count >= limit { return cut ? (data, mime) : nil }
            }
        } catch { return nil }
        return (data, mime)
    }
}
