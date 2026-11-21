import Foundation

/// One line of a permission card's "Kith sees": what the host itself read from the call (host `cards/intent.rs`).
public struct CardFact: Codable, Hashable, Sendable {
    /// terminal, down, globe, pencil, trash, stop, folder, eye or doc.
    public var icon: String
    public var text: String
    /// Parts of `text` drawn in 600, in order.
    public var strong: [String]?
    /// Muted words after the text: whose folder it is.
    public var muted: String?
    /// The app adds this Mac's name after the muted words.
    public var place: Bool?

    public init(icon: String, text: String, strong: [String]? = nil, muted: String? = nil, place: Bool? = nil) {
        self.icon = icon
        self.text = text
        self.strong = strong
        self.muted = muted
        self.place = place
    }

    /// A piece of the text, bold or not.
    public struct Piece: Hashable, Sendable {
        public var text: String
        public var bold: Bool
    }

    /// The text cut into plain and bold pieces: each `strong` part is found in order, so a repeated word bolds the right one.
    public var pieces: [Piece] {
        var out: [Piece] = []
        var at = text.startIndex
        for s in strong ?? [] where !s.isEmpty {
            guard let hit = text.range(of: s, range: at..<text.endIndex) else { continue }
            if hit.lowerBound > at { out.append(Piece(text: String(text[at..<hit.lowerBound]), bold: false)) }
            out.append(Piece(text: s, bold: true))
            at = hit.upperBound
        }
        if at < text.endIndex { out.append(Piece(text: String(text[at...]), bold: false)) }
        return out
    }

    /// The muted words after the text: the host's label, then this Mac for the line that says where it runs.
    public func note(hostName: String) -> String {
        [muted, place == true ? hostName : nil].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " \u{00B7} ")
    }

    /// The SF Symbol for the icon word; one the host adds later draws as a plain document.
    public var symbol: String {
        switch icon {
        case "terminal": "terminal"
        case "down": "arrow.down.circle"
        case "globe": "globe"
        case "pencil": "pencil"
        case "trash": "trash"
        case "stop": "stop.circle"
        case "folder": "folder"
        case "eye": "eye"
        default: "doc"
        }
    }
}

extension EntryData {
    /// The facts to draw, or nil for today's card (a host that sent none, or only lines with no text).
    public var shownFacts: [CardFact]? {
        let kept = (facts ?? []).filter { !$0.text.isEmpty }
        return kept.isEmpty ? nil : kept
    }
}
