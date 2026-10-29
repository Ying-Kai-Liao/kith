import SwiftUI

import UIKit
private extension UIColor {
    convenience init(hex: UInt32, alpha: CGFloat = 1) {
        self.init(
            red: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: alpha
        )
    }
}

public extension Color {
    init(hex: UInt32) {
        self.init(red: Double((hex >> 16) & 0xFF) / 255, green: Double((hex >> 8) & 0xFF) / 255, blue: Double(hex & 0xFF) / 255)
    }

    /// Light/dark pair resolved by the system appearance.
    init(light: UInt32, dark: UInt32) {
        self.init(uiColor: UIColor { $0.userInterfaceStyle == .dark ? UIColor(hex: dark) : UIColor(hex: light) })
    }
}

/// The Kith brand tokens (brand.tokens.json, brand.dark.tokens.json): white or night ground,
/// violet accent, cool greys; amber marks "needs you" and red marks errors.
public enum Palette {
    public static let background = Color(light: 0xFFFFFF, dark: 0x14161B)
    public static let surface = Color(light: 0xF6F6FA, dark: 0x1E2129)
    public static let bubbleAgent = Color(light: 0xF6F6FA, dark: 0x1E2129)
    public static let bubbleUser = Color(light: 0xE7E8EE, dark: 0x2C303A)
    public static let border = Color(light: 0xE7E8EE, dark: 0x2C303A)
    public static let text = Color(light: 0x14161B, dark: 0xFFFFFF)
    public static let secondary = Color(light: 0x6B7280, dark: 0xB4BAC4)
    public static let tertiary = Color(light: 0x6B7280, dark: 0xB4BAC4)
    /// Ink used for fills (primary buttons, the send button, unread badges).
    public static let accentFill = Color(light: 0x6A45F2, dark: 0xB49CFF)
    /// Ink readable as text and tint on the background.
    public static let accent = Color(light: 0x6A45F2, dark: 0xB49CFF)
    public static let onAccent = Color(light: 0xFFFFFF, dark: 0x14161B)
    public static let accentDim = Color(light: 0xE7E8EE, dark: 0x2C303A)
    public static let danger = Color(light: 0xC93450, dark: 0xFF7A8E)
    public static let warning = Color(hex: 0xF0A030)
    /// A bot that needs you, in widgets and Live Activities: readable amber in both modes.
    public static let attention = Color(light: 0x936000, dark: 0xECAF52)
    public static let codeBackground = Color(light: 0xF6F6FA, dark: 0x1E2129)
    public static let added = Color(light: 0x178357, dark: 0x4CC38A)
    public static let removed = Color(light: 0xC93450, dark: 0xFF7A8E)
}

/// The 15 colour pairs a bot's body can wear (eyes.json `palette`, from branding). A pick is
/// stored in `avatarColor` as `kith:<id>` (gradient) or `kith:<id>:plain` (a flat fill of the pair's first colour); any other value, including the bare ids of the old
/// palette (`blue`, `green`...), is the untouched original body.
public enum AvatarPalette {
    public struct Swatch: Identifiable, Hashable, Sendable {
        public let id: String
        public let label: String
        public let a: UInt32
        public let b: UInt32
        /// The value to store in `avatarColor`.
        public var stored: String { "kith:" + id }
        /// The value for a flat fill of `a`.
        public var storedPlain: String { stored + ":plain" }
        public func stored(plain: Bool) -> String { plain ? storedPlain : stored }
        /// The deeper colour of the pair, for badges.
        public var color: Color { Color(hex: a) }
    }

    public static let colors: [Swatch] = [
        .init(id: "blue", label: "Blue", a: 0x6E8BFF, b: 0xC9D2FF),
        .init(id: "sky", label: "Sky", a: 0x6E8BFF, b: 0xBFE3FF),
        .init(id: "cyan", label: "Cyan", a: 0x5ED3F3, b: 0x8FB0FF),
        .init(id: "teal", label: "Teal", a: 0x3FD0C0, b: 0xB8F2E4),
        .init(id: "mint", label: "Mint", a: 0x3FD0C0, b: 0x7EE8C1),
        .init(id: "green", label: "Green", a: 0x7BE29A, b: 0xC8F5D8),
        .init(id: "sun", label: "Sun", a: 0xFFC56B, b: 0xFFE7A6),
        .init(id: "apricot", label: "Apricot", a: 0xFFC56B, b: 0xFDCEC0),
        .init(id: "peach", label: "Peach", a: 0xFFB59A, b: 0xFFB3C7),
        .init(id: "coral", label: "Coral", a: 0xFF7E8E, b: 0xFFD1BF),
        .init(id: "rose", label: "Rose", a: 0xFF5F7A, b: 0xFFC0C8),
        .init(id: "pink", label: "Pink", a: 0xFF7EB0, b: 0xFFC9DC),
        .init(id: "violet", label: "Violet", a: 0x8A5CFF, b: 0xD9C9FF),
        .init(id: "orchid", label: "Orchid", a: 0xA67EFC, b: 0xF281F3),
        .init(id: "dawn", label: "Dawn", a: 0x8FC7FC, b: 0xA67EFC),
    ]

    /// What a stored `avatarColor` picks: `kith:<id>` with a known id is a gradient, `kith:<id>:plain` a flat fill; nil is
    /// the original body. The one parse every drawing goes through.
    public static func parse(_ stored: String?) -> (id: String, plain: Bool)? {
        guard let stored, stored.hasPrefix("kith:") else { return nil }
        var id = String(stored.dropFirst(5))
        var plain = false
        if id.hasSuffix(":plain") { id = String(id.dropLast(6)); plain = true }
        return colors.contains { $0.id == id } ? (id, plain) : nil
    }

    /// The palette id a stored `avatarColor` picks, gradient or plain; nil is the original body.
    public static func pick(_ stored: String?) -> String? { parse(stored)?.id }

    public static func isPlain(_ stored: String?) -> Bool { parse(stored)?.plain ?? false }

    /// A badge color (a computer's, a call's): the pair's deeper colour, with or without the
    /// `kith:` prefix; the accent when it is neither. These are pastels, so never use one as text on white.
    public static func color(_ id: String) -> Color {
        var bare = id.hasPrefix("kith:") ? String(id.dropFirst(5)) : id
        if bare.hasSuffix(":plain") { bare = String(bare.dropLast(6)) }
        return colors.first { $0.id == bare }?.color ?? Palette.accent
    }
}

public enum RelativeTime {
    /// Grok-style compact age: now · 5m · 3h · 2d · Sep 3
    public static func short(_ date: Date, now: Date = .now) -> String {
        let s = max(0, now.timeIntervalSince(date))
        switch s {
        case ..<60: return "now"
        case ..<3600: return "\(Int(s / 60))m"
        case ..<86_400: return "\(Int(s / 3600))h"
        case ..<(86_400 * 7): return "\(Int(s / 86_400))d"
        default: return date.formatted(.dateTime.month(.abbreviated).day())
        }
    }

    /// Roster stamp: 3:45 AM · Yesterday · Wednesday · Sep 16
    public static func day(_ date: Date, now: Date = .now) -> String {
        let cal = Calendar.current
        if cal.isDate(date, inSameDayAs: now) { return date.formatted(date: .omitted, time: .shortened) }
        if cal.isDateInYesterday(date) { return String(localized: "Yesterday") }
        if now.timeIntervalSince(date) < 6 * 86_400 { return date.formatted(.dateTime.weekday(.wide)) }
        return date.formatted(.dateTime.month(.abbreviated).day())
    }

    /// Chat separator: Today 3:27 AM · Yesterday 5:20 PM · Sep 16 9:02 AM
    public static func separator(_ date: Date) -> String {
        let cal = Calendar.current
        let day = cal.isDateInToday(date) ? String(localized: "Today")
            : cal.isDateInYesterday(date) ? String(localized: "Yesterday")
            : date.formatted(.dateTime.month(.abbreviated).day())
        return day + " " + date.formatted(date: .omitted, time: .shortened)
    }

    public static func until(_ date: Date, now: Date = .now) -> String {
        let s = max(0, date.timeIntervalSince(now))
        let h = Int(s / 3600), m = Int(s.truncatingRemainder(dividingBy: 3600) / 60)
        if h >= 48 { return "\(h / 24)d" }
        if h > 0 { return "\(h)h \(m)m" }
        return "\(m)m"
    }
}

public enum BackendInfo {
    public static func name(_ id: String) -> String {
        switch id {
        case "claude": "Claude Code"
        case "codex": "Codex"
        case "opencode": "OpenCode"
        case "grok": "Grok Build"
        case "gemini": "Gemini CLI"
        default: "Custom agent"
        }
    }
}
