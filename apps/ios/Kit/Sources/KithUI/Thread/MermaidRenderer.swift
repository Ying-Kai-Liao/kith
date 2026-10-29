import UIKit
import WebKit

/// Draws ```mermaid diagrams as pictures with the bundled mermaid.js, in one shared offscreen web view: a long chat
/// with many diagrams holds pictures, not a web view per message. Diagrams draw one at a time and are cached by
/// source and light/dark.
///
/// The diagram text comes from agents, invited people and outside bots, so the page is shut:
/// - nothing loads: a content rule list blocks every URL, the CSP is `default-src 'none'`, there is no base URL,
///   and every navigation after the page itself is refused;
/// - nothing of the diagram's runs: page JavaScript is off (only the script injected here runs), mermaid's
///   securityLevel is "strict" with HTML labels off, and a diagram's own config can't change either;
/// - nothing comes back but the picture's size: there is no message handler, the driver returns two numbers, and
///   the picture is a snapshot, so links in a diagram can't be followed.
/// Anything that goes wrong (a parse error, a timeout, a diagram too large) is nil, and the chat shows the code.
@MainActor
final class MermaidRenderer: NSObject, WKNavigationDelegate {
    static let shared = MermaidRenderer()

    /// Past these a fence stays code: a source mermaid would take long on, or a picture that would take a lot of memory.
    static let maxSourceLength = 20_000
    static let maxSide: CGFloat = 4_000
    static let maxArea: CGFloat = 3_000_000
    private static let maxPixels: CGFloat = 8_000_000
    var timeout: Duration = .seconds(5)

    private let cache = NSCache<NSString, UIImage>()
    private var failed = Set<String>()
    private var drawing: [String: Task<UIImage?, Never>] = [:]
    private var tail: Task<Void, Never>?
    private(set) var webView: WKWebView?
    private var navigated = false
    private var loading: CheckedContinuation<Bool, Never>?

    override init() {
        cache.totalCostLimit = 64_000_000
    }

    /// The picture if it's drawn already, so a row scrolled back into view shows it at once.
    func cached(_ source: String, dark: Bool) -> UIImage? {
        cache.object(forKey: Self.key(source, dark) as NSString)
    }

    func image(_ source: String, dark: Bool) async -> UIImage? {
        let key = Self.key(source, dark)
        if let image = cache.object(forKey: key as NSString) { return image }
        if failed.contains(key) { return nil }
        if let task = drawing[key] { return await task.value }
        guard source.count <= Self.maxSourceLength, !source.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            failed.insert(key)
            return nil
        }
        let previous = tail
        let task = Task { () -> UIImage? in
            await previous?.value
            let outcome = await self.draw(source, dark: dark)
            self.drawing[key] = nil
            switch outcome {
            case let .drawn(image):
                self.cache.setObject(image, forKey: key as NSString, cost: Self.bytes(image))
                return image
            case .failed, .timedOut:
                self.failed.insert(key)
                return nil
            case .noPage:
                return nil
            }
        }
        drawing[key] = task
        tail = Task { _ = await task.value }
        return await task.value
    }

    private static func key(_ source: String, _ dark: Bool) -> String { (dark ? "dark\n" : "light\n") + source }

    private static func bytes(_ image: UIImage) -> Int {
        Int(image.size.width * image.scale * image.size.height * image.scale * 4)
    }

    // MARK: - Drawing

    /// `.noPage` isn't the diagram's fault, so it's tried again next time; the others are remembered.
    private enum Outcome { case drawn(UIImage), failed, timedOut, noPage }

    private func draw(_ source: String, dark: Bool) async -> Outcome {
        guard let webView = await page() else { return .noPage }
        let outcome = await within(timeout) { await self.render(source, dark: dark, in: webView) }
        // A diagram that hangs mermaid would hang the next one too: start again with a fresh page.
        if case .timedOut = outcome { discard() }
        return outcome
    }

    private func render(_ source: String, dark: Bool, in webView: WKWebView) async -> Outcome {
        let result = try? await webView.callAsyncJavaScript(
            "return await kithMermaid(source, dark)", arguments: ["source": source, "dark": dark], contentWorld: .page)
        guard let size = result as? [Any], size.count == 2,
              let width = (size[0] as? NSNumber).map({ CGFloat($0.doubleValue) }),
              let height = (size[1] as? NSNumber).map({ CGFloat($0.doubleValue) }),
              width.isFinite, height.isFinite, width >= 1, height >= 1,
              width <= Self.maxSide, height <= Self.maxSide, width * height <= Self.maxArea
        else { return .failed }
        webView.frame = CGRect(x: 0, y: 0, width: width, height: height)
        let config = WKSnapshotConfiguration()
        config.rect = CGRect(x: 0, y: 0, width: width, height: height)
        // At most 2 pixels a point, fewer for a large diagram.
        let scale = min(2, (Self.maxPixels / (width * height)).squareRoot())
        config.snapshotWidth = NSNumber(value: Double(width * scale / webView.traitCollection.displayScale.clamped))
        guard let shot = try? await webView.takeSnapshot(configuration: config), let cg = shot.cgImage else { return .failed }
        return .drawn(UIImage(cgImage: cg, scale: CGFloat(cg.width) / width, orientation: .up))
    }

    /// Runs `work`, or gives up after `limit`. The web view can't cancel a script, so the race is by hand.
    private func within(_ limit: Duration, _ work: @escaping @MainActor () async -> Outcome) async -> Outcome {
        final class Once {
            var continuation: CheckedContinuation<Outcome, Never>?
        }
        let once = Once()
        return await withCheckedContinuation { continuation in
            once.continuation = continuation
            Task { @MainActor in
                let outcome = await work()
                once.continuation?.resume(returning: outcome)
                once.continuation = nil
            }
            Task { @MainActor in
                try? await Task.sleep(for: limit)
                once.continuation?.resume(returning: .timedOut)
                once.continuation = nil
            }
        }
    }

    // MARK: - The page

    private func page() async -> WKWebView? {
        if let webView { return webView }
        guard let url = Bundle.module.url(forResource: "mermaid.min", withExtension: "js"),
              let mermaid = try? String(contentsOf: url, encoding: .utf8),
              let rules = try? await WKContentRuleListStore.default().compileContentRuleList(
                  forIdentifier: "kith-mermaid-block-all", encodedContentRuleList: Self.blockEverything)
        else { return nil }
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.defaultWebpagePreferences.allowsContentJavaScript = false
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        config.dataDetectorTypes = []
        config.userContentController.add(rules)
        config.userContentController.addUserScript(
            WKUserScript(source: mermaid + "\n" + Self.driver, injectionTime: .atDocumentEnd, forMainFrameOnly: true))
        let webView = WKWebView(frame: CGRect(x: 0, y: 0, width: 800, height: 600), configuration: config)
        webView.isOpaque = false
        webView.backgroundColor = .clear
        webView.scrollView.backgroundColor = .clear
        webView.scrollView.contentInsetAdjustmentBehavior = .never
        webView.navigationDelegate = self
        navigated = false
        self.webView = webView
        let loaded = await withCheckedContinuation { continuation in
            loading = continuation
            webView.loadHTMLString(Self.html, baseURL: nil)
        }
        // The first diagram on a fresh page is slow while mermaid warms up (seconds on an older phone), so one is
        // drawn here, with time to spare, rather than charged to a real diagram's timeout.
        guard loaded, self.webView === webView,
              case .drawn = await within(.seconds(30), { await self.render("graph TD\nA", dark: false, in: webView) }),
              self.webView === webView
        else {
            discard()
            return nil
        }
        return webView
    }

    private func discard() {
        webView?.navigationDelegate = nil
        webView?.stopLoading()
        webView = nil
        finishLoading(false)
    }

    private func finishLoading(_ ok: Bool) {
        loading?.resume(returning: ok)
        loading = nil
    }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
        // The page itself, once; a link, a form, a redirect or a script can't take it anywhere after that.
        guard webView === self.webView, !navigated else { return .cancel }
        navigated = true
        return .allow
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        if webView === self.webView { finishLoading(true) }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        if webView === self.webView { finishLoading(false) }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        if webView === self.webView { finishLoading(false) }
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        if webView === self.webView { discard() }
    }

    private static let blockEverything = #"[{"trigger":{"url-filter":".*"},"action":{"type":"block"}}]"#

    private static let html = """
    <!doctype html><html><head><meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'">
    <style>html,body{margin:0;padding:0;background:transparent}#d{display:inline-block}</style>
    </head><body><div id="d"></div></body></html>
    """

    /// Draws one diagram into #d at its natural size and returns [width, height]; throws when mermaid can't.
    /// The keys in `secure` can't be changed by a diagram's own config (an init directive or front matter).
    private static let driver = """
    (() => {
      const secure = ['secure', 'securityLevel', 'startOnLoad', 'maxTextSize', 'maxEdges', 'suppressErrorRendering',
        'htmlLabels', 'flowchart', 'theme', 'themeVariables', 'themeCSS', 'fontFamily', 'altFontFamily',
        'dompurifyConfig', 'deterministicIds', 'arrowMarkerAbsolute']
      let n = 0
      window.kithMermaid = async (source, dark) => {
        const d = document.getElementById('d')
        for (const e of [...document.body.children]) if (e !== d) e.remove()
        d.replaceChildren()
        mermaid.initialize({
          startOnLoad: false, securityLevel: 'strict', htmlLabels: false, flowchart: { htmlLabels: false },
          theme: dark ? 'dark' : 'default', fontFamily: '-apple-system, system-ui, sans-serif',
          suppressErrorRendering: true, secure,
        })
        const { svg } = await mermaid.render('kith-mermaid-' + ++n, source)
        d.innerHTML = svg
        const el = d.querySelector('svg')
        const box = el.viewBox && el.viewBox.baseVal
        const rect = el.getBoundingClientRect()
        const w = Math.ceil(box && box.width ? box.width : rect.width)
        const h = Math.ceil(box && box.height ? box.height : rect.height)
        el.removeAttribute('style')
        el.setAttribute('width', w)
        el.setAttribute('height', h)
        return [w, h]
      }
    })()
    """
}

private extension CGFloat {
    /// A display scale to divide by: the trait can be 0 before the view has a screen.
    var clamped: CGFloat { self > 0 ? self : 1 }
}
