import KithKit
import SwiftUI
import UIKit

/// "Select Text" from a message's menu: the message in a text view where a drag picks any part of it.
/// The bubble itself can't start a selection, since its long-press opens the menu.
struct SelectTextSheet: View {
    let text: String

    var body: some View {
        SelectableText(text: text)
            .padding(.horizontal, KithSpacing.gutter)
            .background(Palette.background)
            .presentationDetents([.medium, .large])
    }
}

private struct SelectableText: UIViewRepresentable {
    let text: String

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.isEditable = false
        view.isSelectable = true
        view.backgroundColor = .clear
        view.textColor = UIColor(Palette.ink)
        let body = UIFont(name: KithFonts.interRegular, size: KithText.body.size) ?? .systemFont(ofSize: KithText.body.size)
        view.font = UIFontMetrics(forTextStyle: .subheadline).scaledFont(for: body)
        view.adjustsFontForContentSizeCategory = true
        view.dataDetectorTypes = .link
        view.textContainerInset = UIEdgeInsets(top: 28, left: 0, bottom: 28, right: 0)
        view.textContainer.lineFragmentPadding = 0
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        if view.text != text { view.text = text }
    }
}

extension MarkdownText {
    /// A reply as it reads on screen, without Markdown marks: what "Select Text" shows.
    static func plain(_ source: String) -> String {
        var out = ""
        var previous: MarkdownBlocks.Block?
        for block in MarkdownBlocks.parse(source) {
            let line: String
            switch block {
            case let .paragraph(t), let .heading(t, _), let .quote(t):
                line = flat(t)
            case let .bullet(t, marker, depth):
                line = String(repeating: "    ", count: depth) + marker + " " + flat(t)
            case let .code(t, _):
                line = t
            case let .table(header, rows):
                line = ([header] + rows).map { $0.map(flat).joined(separator: "\t") }.joined(separator: "\n")
            case .rule:
                continue
            }
            if previous != nil {
                if case .bullet = block, case .bullet = previous { out += "\n" } else { out += "\n\n" }
            }
            out += line
            previous = block
        }
        return out
    }

    private static func flat(_ s: String) -> String { String(inline(s).characters) }
}
