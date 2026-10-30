import KithKit
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// "Kith" in another app's share sheet: pick a chat, add a note, and the link is queued in the App Group. The app
/// sends it when it next opens (KithApp.deliverShares); this process has no connection to the computer.
final class ShareViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        let items = (extensionContext?.inputItems as? [NSExtensionItem]) ?? []
        Task { @MainActor in
            let content = await ShareContent.load(from: items)
            let composer = ShareComposer(
                content: content,
                context: SharedStore.activeContext,
                finish: { [weak self] in self?.extensionContext?.completeRequest(returningItems: nil) },
                cancel: { [weak self] in
                    self?.extensionContext?.cancelRequest(withError: CocoaError(.userCancelled))
                }
            )
            let host = UIHostingController(rootView: composer)
            addChild(host)
            host.view.frame = view.bounds
            host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            view.addSubview(host.view)
            host.didMove(toParent: self)
        }
    }
}

/// What the host app handed over: a link and/or text, and the page title when it sent one.
struct ShareContent {
    var link: URL?
    var title: String?
    var text: String?

    var isEmpty: Bool { link == nil && (text ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    // Main-actor isolated so the non-Sendable NSExtensionItem array never crosses actors (Swift 6 strict concurrency).
    @MainActor
    static func load(from items: [NSExtensionItem]) async -> ShareContent {
        var content = ShareContent()
        content.title = items.compactMap { $0.attributedTitle?.string }.first
        for provider in items.flatMap({ $0.attachments ?? [] }) {
            if content.link == nil, provider.hasItemConformingToTypeIdentifier(UTType.url.identifier),
               let item = try? await provider.loadItem(forTypeIdentifier: UTType.url.identifier),
               let url = (item as? URL) ?? (item as? String).flatMap(URL.init(string:)), SharedLink.isWeb(url) {
                content.link = url
            }
            if content.text == nil, provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier),
               let item = try? await provider.loadItem(forTypeIdentifier: UTType.plainText.identifier), let text = item as? String {
                content.text = text
            }
        }
        // A link shared as plain text ("Look at this https://…").
        if content.link == nil, let text = content.text ?? items.compactMap({ $0.attributedContentText?.string }).first {
            content.link = SharedLink.firstURL(in: text)
            content.text = content.text ?? text
        }
        return content
    }
}
