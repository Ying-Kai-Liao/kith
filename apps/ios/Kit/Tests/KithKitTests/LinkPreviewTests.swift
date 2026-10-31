import CoreGraphics
import Foundation
import ImageIO
import Testing
import UniformTypeIdentifiers
@testable import KithKit

private let page = URL(string: "https://news.example.com/a/story")!

@Test func theHeadNamesOgImageThenTwitterThenTouchIcon() {
    let html = """
    <html><head><meta property="og:image" content="/img/og.jpg"><meta name="twitter:image" content="https://cdn.example.com/t.jpg">
    <link rel="apple-touch-icon" href="/touch.png"><link rel="icon" href="/fav.png"></head><body><meta property="og:image" content="/late.jpg"></body></html>
    """
    let head = LinkPreview.parse(html: html, base: page)
    #expect(head.image == URL(string: "https://news.example.com/img/og.jpg"))
    #expect(head.icon == URL(string: "https://news.example.com/fav.png"))
    let t = LinkPreview.parse(html: "<head><meta name='twitter:image' content='//cdn.example.com/t.jpg'><link rel='apple-touch-icon' href='touch.png'></head>", base: page)
    #expect(t.image == URL(string: "https://cdn.example.com/t.jpg"))
    let touch = LinkPreview.parse(html: "<head><link rel=\"apple-touch-icon\" href=\"touch.png\"></head>", base: page)
    #expect(touch.image == URL(string: "https://news.example.com/a/touch.png"))
}

@Test func theFaviconFallsBackToTheRootAndEntitiesAreDecoded() {
    let head = LinkPreview.parse(html: "<head><META PROPERTY=\"og:image\" CONTENT=\"/i.jpg?a=1&amp;b=2\"></head>", base: page)
    #expect(head.image == URL(string: "https://news.example.com/i.jpg?a=1&b=2"))
    #expect(head.icon == URL(string: "https://news.example.com/favicon.ico"))
    #expect(LinkPreview.parse(html: "", base: page).image == nil)
}

@Test func pictureLinksToPrivateHostsOrOtherSchemesAreDropped() {
    let head = LinkPreview.parse(html: "<head><meta property=\"og:image\" content=\"http://192.168.1.5/x.jpg\"><meta name=\"twitter:image\" content=\"data:image/png;base64,AAAA\"></head>", base: page)
    #expect(head.image == nil)
}

@Test func privateHostsAreRefused() {
    for host in ["localhost", "foo.local", "127.0.0.1", "127.8.8.8", "10.1.2.3", "192.168.0.9", "172.16.0.1", "172.31.255.1",
                 "169.254.169.254", "0.0.0.0", "[::1]", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1", "intranet", "100.64.0.1"] {
        #expect(LinkPreview.isPrivate(host: host), "\(host)")
    }
    for host in ["bom.gov.au", "172.32.0.1", "172.15.0.1", "8.8.8.8", "192.169.0.1", "2606:4700::1111"] {
        #expect(!LinkPreview.isPrivate(host: host), "\(host)")
    }
    #expect(!LinkPreview.isFetchable(URL(string: "ftp://bom.gov.au/x")!))
    #expect(!LinkPreview.isFetchable(URL(string: "http://localhost:8080/x")!))
    #expect(LinkPreview.isFetchable(URL(string: "http://bom.gov.au/x")!))
}

// MARK: Store

private func png(_ side: Int) -> Data {
    let ctx = CGContext(data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0,
                        space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let out = NSMutableData()
    let dest = CGImageDestinationCreateWithData(out, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, ctx.makeImage()!, nil)
    CGImageDestinationFinalize(dest)
    return out as Data
}

private final class FakeLoader: LinkPreviewLoading, @unchecked Sendable {
    let lock = NSLock()
    var pages = 0
    var html: String?
    var images: [String: Data] = [:]
    func page(_ url: URL) async -> String? {
        lock.withLock { pages += 1 }
        try? await Task.sleep(for: .milliseconds(30))
        return html
    }
    func image(_ url: URL) async -> Data? { images[url.absoluteString] }
}

private func tempDir() -> URL {
    FileManager.default.temporaryDirectory.appendingPathComponent("lp-\(UUID().uuidString)")
}

private final class Clock: @unchecked Sendable {
    var date = Date(timeIntervalSince1970: 1_800_000_000)
}

private let goodHTML = "<head><meta property=\"og:image\" content=\"/p.png\"><link rel=\"icon\" href=\"/i.png\"></head>"
private func goodLoader() -> FakeLoader {
    let l = FakeLoader()
    l.html = goodHTML
    l.images = ["https://news.example.com/p.png": png(100), "https://news.example.com/i.png": png(16)]
    return l
}

@Test func aLinkIsFetchedOnceWhateverTheCallersAndThenServedFromCache() async {
    let loader = goodLoader()
    let store = LinkPreviewStore(directory: tempDir(), loader: loader)
    async let a = store.images(for: page)
    async let b = store.images(for: page)
    let (ra, rb) = await (a, b)
    #expect(ra?.picture != nil && ra?.icon != nil)
    #expect(ra == rb)
    _ = await store.images(for: page)
    #expect(loader.lock.withLock { loader.pages } == 1)
}

@Test func theDiskCacheSurvivesANewStore() async {
    let dir = tempDir()
    let loader = goodLoader()
    _ = await LinkPreviewStore(directory: dir, loader: loader).images(for: page)
    let again = await LinkPreviewStore(directory: dir, loader: loader).images(for: page)
    #expect(again?.picture != nil)
    #expect(loader.lock.withLock { loader.pages } == 1)
}

@Test func aSmallPictureCountsAsNoneButTheFaviconStays() async {
    let loader = goodLoader()
    loader.images["https://news.example.com/p.png"] = png(60)
    let r = await LinkPreviewStore(directory: tempDir(), loader: loader).images(for: page)
    #expect(r?.picture == nil)
    #expect(r?.icon != nil)
}

@Test func aMissIsRememberedUntilItExpires() async {
    let clock = Clock()
    let loader = FakeLoader() // no html: the fetch fails
    let store = LinkPreviewStore(directory: tempDir(), loader: loader, now: { clock.date })
    let first = await store.images(for: page)
    #expect(first == LinkPreviewImages.none)
    // A fresh store reads the negative entry from disk instead of asking again.
    let dir = tempDir()
    let s1 = LinkPreviewStore(directory: dir, loader: loader, now: { clock.date })
    _ = await s1.images(for: page)
    let before = loader.lock.withLock { loader.pages }
    _ = await LinkPreviewStore(directory: dir, loader: loader, now: { clock.date }).images(for: page)
    #expect(loader.lock.withLock { loader.pages } == before)
    clock.date.addTimeInterval(8 * 24 * 3600)
    _ = await LinkPreviewStore(directory: dir, loader: loader, now: { clock.date }).images(for: page)
    #expect(loader.lock.withLock { loader.pages } == before + 1)
}

@Test func withTheSwitchOffNothingIsFetched() async {
    let loader = goodLoader()
    let store = LinkPreviewStore(directory: tempDir(), loader: loader, enabled: { false })
    #expect(await store.images(for: page) == nil)
    store.warm([page])
    try? await Task.sleep(for: .milliseconds(100))
    #expect(loader.lock.withLock { loader.pages } == 0)
}

@Test func aPrivatePageIsNeverAsked() async {
    let loader = goodLoader()
    let store = LinkPreviewStore(directory: tempDir(), loader: loader)
    #expect(await store.images(for: URL(string: "http://192.168.1.1/x")!) == nil)
    #expect(loader.lock.withLock { loader.pages } == 0)
}
