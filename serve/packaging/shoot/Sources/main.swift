import AppKit
import Foundation
import WebKit

// kith-shoot <page.html> <out.png>: loads the page in an offscreen web view and writes a 720x420 picture (1440x840
// pixels, 2x) of its `[data-preview]` element, else of the first 720x420 CSS pixels (the viewport is 720x420, so text
// reads at a normal size on the card).
//
// The page is untrusted: nothing persists (non-persistent data store), every http(s) and ws(s) load is blocked by a
// content rule list, and files can be read only from the page's own folder. JS stays on because pages draw with it.
// One try: after 10 seconds the process exits with an error.

let viewWidth: CGFloat = 720
let viewHeight: CGFloat = 420
let outWidth = 1440
let outHeight = 840
let timeout: TimeInterval = 10
let maxRectHeight: CGFloat = 8000
let rulesDir = FileManager.default.temporaryDirectory.appendingPathComponent("kith-shoot-rules-\(getpid())")

func quit(_ code: Int32) -> Never {
    try? FileManager.default.removeItem(at: rulesDir)
    exit(code)
}

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("kith-shoot: \(message)\n".utf8))
    quit(1)
}

let args = CommandLine.arguments
guard args.count == 3 else { fail("usage: kith-shoot <page.html> <out.png>") }
let page = URL(fileURLWithPath: args[1]).standardizedFileURL
let out = URL(fileURLWithPath: args[2])
guard FileManager.default.isReadableFile(atPath: page.path) else { fail("can't read \(page.path)") }

let blockRules = """
[{"trigger":{"url-filter":"^https?://"},"action":{"type":"block"}},
 {"trigger":{"url-filter":"^wss?://"},"action":{"type":"block"}}]
"""

/// Draws `image` to fill 1440x840 pixels, scaled to cover and cropped at the centre.
func cover(_ image: CGImage) -> Data? {
    guard let ctx = CGContext(
        data: nil, width: outWidth, height: outHeight, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
    else { return nil }
    ctx.setFillColor(NSColor.white.cgColor)
    ctx.fill(CGRect(x: 0, y: 0, width: outWidth, height: outHeight))
    ctx.interpolationQuality = .high
    let w = CGFloat(image.width), h = CGFloat(image.height)
    let scale = max(CGFloat(outWidth) / w, CGFloat(outHeight) / h)
    let dw = w * scale, dh = h * scale
    ctx.draw(image, in: CGRect(x: (CGFloat(outWidth) - dw) / 2, y: (CGFloat(outHeight) - dh) / 2, width: dw, height: dh))
    guard let result = ctx.makeImage() else { return nil }
    return NSBitmapImageRep(cgImage: result).representation(using: .png, properties: [:])
}

final class Shooter: NSObject, WKNavigationDelegate {
    let web: WKWebView
    let window: NSWindow

    init(rules: WKContentRuleList) {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.userContentController.add(rules)
        web = WKWebView(frame: CGRect(x: 0, y: 0, width: viewWidth, height: viewHeight), configuration: config)
        // A web view that is in no window draws nothing; this one sits far off screen and takes no input.
        window = NSWindow(contentRect: CGRect(x: -20000, y: -20000, width: viewWidth, height: viewHeight),
                          styleMask: .borderless, backing: .buffered, defer: false)
        window.contentView = web
        window.ignoresMouseEvents = true
        super.init()
        web.navigationDelegate = self
        window.orderBack(nil)
    }

    func start() {
        web.loadFileURL(page, allowingReadAccessTo: page.deletingLastPathComponent())
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        // Pages draw with JS and fonts after load: give them a moment.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { self.measure() }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        fail("page failed: \(error.localizedDescription)")
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        fail("page didn't load: \(error.localizedDescription)")
    }

    func measure() {
        let js = """
        (() => { const el = document.querySelector('[data-preview]'); if (!el) return null;
          const r = el.getBoundingClientRect();
          return [r.left + window.scrollX, r.top + window.scrollY, r.width, r.height]; })()
        """
        web.evaluateJavaScript(js) { value, _ in
            var rect = CGRect(x: 0, y: 0, width: viewWidth, height: viewHeight)
            if let v = value as? [Double], v.count == 4, v[2] >= 8, v[3] >= 8 {
                rect = CGRect(x: v[0], y: v[1], width: v[2], height: min(v[3], maxRectHeight))
            }
            self.shoot(rect)
        }
    }

    func shoot(_ rect: CGRect) {
        // The window must reach the rect's bottom edge for it to be drawn.
        let height = max(viewHeight, min(rect.maxY, maxRectHeight + 1000))
        window.setContentSize(CGSize(width: viewWidth, height: height))
        web.frame = CGRect(x: 0, y: 0, width: viewWidth, height: height)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
            let snap = WKSnapshotConfiguration()
            snap.rect = rect
            snap.snapshotWidth = NSNumber(value: Double(self.snapshotWidth(for: rect)))
            snap.afterScreenUpdates = true
            self.web.takeSnapshot(with: snap) { image, error in
                guard let image, let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
                    fail("no picture: \(error?.localizedDescription ?? "empty")")
                }
                guard let png = cover(cg) else { fail("couldn't encode the picture") }
                do { try png.write(to: out, options: .atomic) } catch { fail("couldn't write \(out.path): \(error)") }
                quit(0)
            }
        }
    }

    /// Wide enough that the picture has at least the 1440x840 pixels it is cropped to (the snapshot is 2x).
    func snapshotWidth(for rect: CGRect) -> CGFloat {
        let scale = max(CGFloat(outWidth) / rect.width, CGFloat(outHeight) / rect.height)
        return ceil(rect.width * scale / 2)
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)

DispatchQueue.main.asyncAfter(deadline: .now() + timeout) { fail("timed out after \(Int(timeout)) seconds") }

guard let store = WKContentRuleListStore(url: rulesDir) else { fail("no rule store") }
var shooter: Shooter?
store.compileContentRuleList(forIdentifier: "block-net", encodedContentRuleList: blockRules) { list, error in
    guard let list else { fail("couldn't compile the block rules: \(error?.localizedDescription ?? "unknown")") }
    shooter = Shooter(rules: list)
    shooter?.start()
}
app.run()
