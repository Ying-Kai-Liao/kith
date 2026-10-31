import CoreText
import SwiftUI
import UIKit

// The Kith app design tokens (app-style/DESIGN.md): colours, type, radii, spacing and sizes.
// `Palette` keeps the older names; these add the redesign's.

public extension Palette {
    /// Text and primary buttons. White on the night ground.
    static let ink = Color(light: 0x14161B, dark: 0xFFFFFF)
    static let muted = Color(light: 0x6B7280, dark: 0xB4BAC4)
    /// Secondary text over the sky.
    static let mutedOnSky = Color(light: 0x4B5160, dark: 0xC3C8D2)
    /// Secondary ink: the Info card's sub line and link address (darker than muted).
    static let ink2 = Color(light: 0x3A3D45, dark: 0xD5D9E0)
    /// The Info card's link arrow.
    static let faint = Color(light: 0xB4B8C2, dark: 0x5B6170)
    static let line = Color(light: 0xECEDF2, dark: 0x2C303A)
    static let panel = Color(light: 0xF6F6FA, dark: 0x1E2129)
    /// The lighter violet, for accent text and fills on dark grounds.
    static let accentDark = Color(hex: 0xB49CFF)
    static let attentionBg = Color(light: 0xFFF1DC, dark: 0x3A2A12)
    static let positive = Color(light: 0x178357, dark: 0x4CC38A)
    static let negative = Color(light: 0xC93450, dark: 0xFF7A8E)
    /// The dark ground, in both appearances.
    static let night = Color(hex: 0x14161B)
    /// Floating glass controls below iOS 26: white at 58% (dark: the agent bubble colour).
    static let glassTint = Color(light: 0xFFFFFF, dark: 0x1E2129).opacity(0.58)
}

public enum KithRadius {
    public static let chip: CGFloat = 999
    public static let rowTool: CGFloat = 10
    public static let card: CGFloat = 18
    public static let sheet: CGFloat = 36
    public static let glassPill: CGFloat = 28
}

public enum KithSpacing {
    public static let gutter: CGFloat = 16
    public static let rowV: CGFloat = 9
    public static let stack: CGFloat = 10
}

public enum KithSize {
    public static let botAvatar: CGFloat = 48
    public static let botAvatarTopBar: CGFloat = 30
    public static let botAvatarThread: CGFloat = 24
    public static let botAvatarColourSheet: CGFloat = 84
    public static let chatBoxHeight: CGFloat = 54
    public static let topButton: CGFloat = 42
    public static let toolRowHeight: CGFloat = 44
    public static let countBadge: CGFloat = 18
    public static let countBadgeBoard: CGFloat = 16
    /// The agent chat's sky strip fades out by here.
    public static let skyHeaderFade: CGFloat = 170
}

/// Bundled fonts (Inter, Nunito, JetBrains Mono; SIL OFL 1.1). The files ship in KithUI's resources so the
/// widget and notification extensions stay small; the app registers them at launch, and a font that is not
/// registered (an extension, a failed load) falls back to the system's.
public enum KithFonts {
    public static let interRegular = "Inter-Regular"
    public static let interSemiBold = "Inter-SemiBold"
    public static let interBold = "Inter-Bold"
    public static let nunitoExtraBold = "Nunito-ExtraBold"
    public static let jetBrainsMono = "JetBrainsMono-Regular"

    private static let files = ["Inter-Regular", "Inter-SemiBold", "Inter-Bold", "Nunito-Variable", "JetBrainsMono-Regular"]
    private nonisolated(unsafe) static var registered = false
    private static let lock = NSLock()

    /// Registers the font files found in `bundle` for this process. Calling it again does nothing.
    public static func register(from bundle: Bundle) {
        lock.lock(); defer { lock.unlock() }
        guard !registered else { return }
        registered = true
        for file in files {
            guard let url = bundle.url(forResource: file, withExtension: "ttf", subdirectory: "Fonts")
                ?? bundle.url(forResource: file, withExtension: "ttf") else { continue }
            CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
        }
    }

    static func isAvailable(_ postScriptName: String) -> Bool {
        UIFont(name: postScriptName, size: 12) != nil
    }
}

/// The type ramp. Every style scales with Dynamic Type; `lineHeight` and `tracking` are applied by `kithText`.
public enum KithText: CaseIterable, Sendable {
    case agentName, topBar, body, secondary, label, code

    public var size: CGFloat {
        switch self {
        case .agentName, .topBar: 16
        case .body: 15
        case .secondary: 13
        case .label: 11
        case .code: 12
        }
    }

    public var style: Font.TextStyle {
        switch self {
        case .agentName, .topBar: .body
        case .body: .subheadline
        case .secondary: .footnote
        case .label: .caption2
        case .code: .caption
        }
    }

    /// Line height as a multiple of the size; nil keeps the font's own.
    public var lineHeight: CGFloat? {
        switch self {
        case .agentName: 1.2
        case .body: 1.45
        default: nil
        }
    }

    /// Letter spacing in points (label: 0.06em).
    public var tracking: CGFloat { self == .label ? 11 * 0.06 : 0 }

    public var font: Font {
        switch self {
        case .agentName:
            KithFonts.isAvailable(KithFonts.nunitoExtraBold)
                ? .custom(KithFonts.nunitoExtraBold, size: size, relativeTo: style)
                : .system(size: size, weight: .heavy, design: .rounded)
        case .topBar:
            Self.inter(KithFonts.interSemiBold, size, style, .semibold)
        case .body, .secondary:
            Self.inter(KithFonts.interRegular, size, style, .regular)
        case .label:
            Self.inter(KithFonts.interBold, size, style, .bold)
        case .code:
            KithFonts.isAvailable(KithFonts.jetBrainsMono)
                ? .custom(KithFonts.jetBrainsMono, size: size, relativeTo: style)
                : .system(size: size, design: .monospaced)
        }
    }

    private static func inter(_ name: String, _ size: CGFloat, _ style: Font.TextStyle, _ weight: Font.Weight) -> Font {
        KithFonts.isAvailable(name)
            ? .custom(name, size: size, relativeTo: style)
            : .system(size: size, weight: weight)
    }
}

private struct KithTextModifier: ViewModifier {
    let text: KithText
    @ScaledMetric private var extraLeading: CGFloat

    init(_ text: KithText) {
        self.text = text
        // lineSpacing adds to the font's natural height (about 1.2 em for Inter and Nunito).
        let extra = max(0, (text.lineHeight ?? 0) - 1.2) * text.size
        _extraLeading = ScaledMetric(wrappedValue: extra, relativeTo: text.style)
    }

    func body(content: Content) -> some View {
        content
            .font(text.font)
            .tracking(text.tracking)
            .lineSpacing(text.lineHeight == nil ? 0 : extraLeading)
    }
}

public extension View {
    /// A style from the type ramp: font, tracking and line height.
    func kithText(_ text: KithText) -> some View { modifier(KithTextModifier(text)) }

    /// A bot's soft drop shadow (0 2 4, 12% ink), so it sits on white or sky without a tile.
    func kithBotShadow() -> some View {
        shadow(color: Color(hex: 0x14161B).opacity(0.12), radius: 4, x: 0, y: 2)
    }
}
