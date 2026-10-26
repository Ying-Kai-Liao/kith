import KithKit
import SwiftUI

/// What a bot's face says at a glance.
public enum BotAvatarState: Equatable, Sendable {
    case idle
    /// Hop-dots badge, lower right.
    case working
    /// A count badge for the questions waiting on you.
    case needsYou(Int)
    /// Faded.
    case asleep

    var mood: CharacterAvatar.Mood {
        switch self {
        case .working: .working
        case .needsYou: .needsInput
        case .idle, .asleep: .idle
        }
    }
}

/// A bot, no tile, with a soft drop shadow. Wraps the living bot (`CharacterAvatar`); sizes are `KithSize`
/// (48 rows, 30 top bar, 24 thread chip, 84 colour sheet). A body whose `color` is not `kith:<id>` renders as
/// the untouched original, which is the AvatarPalette rule.
public struct BotAvatar: View {
    let portrait: String?
    let color: String
    let id: String
    let size: CGFloat
    let state: BotAvatarState
    let showsBadge: Bool

    public init(portrait: String?, color: String = "", id: String = "", size: CGFloat = KithSize.botAvatar, state: BotAvatarState = .idle, showsBadge: Bool = true) {
        self.showsBadge = showsBadge
        self.portrait = portrait
        self.color = color
        self.id = id
        self.size = size
        self.state = state
    }

    public init(bot: Bot, size: CGFloat = KithSize.botAvatar, state: BotAvatarState = .idle) {
        self.init(portrait: CrewAvatar.asset(for: bot), color: bot.avatarColor, id: bot.id, size: size, state: state)
    }

    public var body: some View {
        CharacterAvatar(portrait: portrait, color: color, size: size, mood: state.mood, id: id)
            .kithBotShadow()
            .saturation(state == .asleep ? 0.5 : 1)
            .opacity(state == .asleep ? 0.5 : 1)
            .overlay(alignment: .bottomTrailing) {
                if state == .working { WorkingDots(size: size).offset(x: size * 0.04, y: size * 0.06) }
            }
            .overlay(alignment: .topTrailing) {
                // Under 30 pt a badge would cover the face; the row or chip carries the count there.
                if showsBadge, case .needsYou(let n) = state, size >= KithSize.botAvatarTopBar {
                    CountBadge(n, size: KithSize.countBadgeBoard).offset(x: 4, y: -4)
                }
            }
            .frame(width: size, height: size)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(Self.label(state)))
    }

    static func label(_ state: BotAvatarState) -> String {
        switch state {
        case .idle: "Idle"
        case .working: "Working"
        case .needsYou(let n): n > 0 ? "Needs you, \(n) waiting" : "Needs you"
        case .asleep: "Asleep"
        }
    }
}

/// Three hopping dots: the bot is working.
struct WorkingDots: View {
    let size: CGFloat
    @State private var up = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let dot = max(2.5, size * 0.065)
        HStack(spacing: dot * 0.6) {
            ForEach(0..<3, id: \.self) { i in
                Circle().fill(Palette.ink).frame(width: dot, height: dot)
                    .offset(y: up && !reduceMotion ? -dot : 0)
                    .animation(reduceMotion ? nil : .easeInOut(duration: 0.45).repeatForever(autoreverses: true).delay(Double(i) * 0.15), value: up)
            }
        }
        .padding(.horizontal, dot * 0.7).padding(.vertical, dot * 0.55)
        .background(Palette.background, in: Capsule())
        .shadow(color: Color(hex: 0x14161B).opacity(0.12), radius: 2, y: 1)
        .onAppear { up = true }
    }
}

/// The tone of a person's sky patch.
public enum SkyTone: CaseIterable, Sendable, CustomStringConvertible {
    case dawn, clear, dusk

    public var description: String {
        switch self { case .dawn: "dawn"; case .clear: "clear"; case .dusk: "dusk" }
    }

    var colours: [Color] {
        switch self {
        case .dawn: [Color(hex: 0x4F86E0), Color(hex: 0xF5A878)]
        case .clear: [Color(hex: 0x1F5FD0), Color(hex: 0x6DB8F5)]
        case .dusk: [Color(hex: 0x5A3FA8), Color(hex: 0xF08A6A)]
        }
    }

    /// FNV-1a: `hashValue` changes every launch, and a person's picture must not.
    static func hash(_ id: String) -> UInt64 {
        var h: UInt64 = 0xcbf29ce484222325
        for byte in id.utf8 { h = (h ^ UInt64(byte)) &* 0x100000001b3 }
        return h
    }

    /// Stable per account id; you are always dawn.
    public static func tone(for id: String, isYou: Bool = false) -> SkyTone {
        isYou ? .dawn : allCases[Int(hash(id) % UInt64(allCases.count))]
    }

    /// Where the cloud crop sits in the sky, 0...1 each way, also stable per id.
    static func crop(for id: String) -> (x: Double, y: Double) {
        let h = hash(id)
        return (Double(h >> 8 & 0xFF) / 255, Double(h >> 16 & 0xFF) / 255)
    }
}

/// One of the Kith sky pictures a person can pick as their own. Tone and crop mirror desktop's `SkyPick`.
public struct SkyPreset: Identifiable, Equatable, Sendable {
    public let tone: SkyTone
    /// Crop position in percent of the sky image.
    public let x: Int, y: Int
    public var id: String { "\(tone)-\(x)-\(y)" }

    public static let all: [SkyPreset] = [
        .init(tone: .dawn, x: 20, y: 30), .init(tone: .dawn, x: 70, y: 60),
        .init(tone: .clear, x: 15, y: 20), .init(tone: .clear, x: 60, y: 40), .init(tone: .clear, x: 85, y: 75),
        .init(tone: .dusk, x: 25, y: 50), .init(tone: .dusk, x: 75, y: 30), .init(tone: .dusk, x: 50, y: 85),
    ]

    var crop: (x: Double, y: Double) { (Double(x) / 100, Double(y) / 100) }

    /// A square picture of the preset, ready to upload.
    @MainActor public func renderedJPEG(side: CGFloat = 512) -> Data? {
        let renderer = ImageRenderer(content: SkyArt(tone: tone, crop: crop, size: side))
        renderer.scale = 1
        return renderer.uiImage?.jpegData(compressionQuality: 0.9)
    }
}

/// The square sky picture behind `SkyPatch` and the uploaded presets.
struct SkyArt: View {
    let tone: SkyTone
    let crop: (x: Double, y: Double)
    let size: CGFloat

    var body: some View {
        ZStack {
            LinearGradient(colors: tone.colours, startPoint: .top, endPoint: .bottom)
            Image("sky-home-day", bundle: KithUIFonts.resources)
                .resizable().scaledToFill()
                .frame(width: size * 3, height: size * 3)
                .offset(x: (crop.x - 0.5) * size * 2, y: (crop.y - 0.5) * size * 2)
                .opacity(0.14)
        }
        .frame(width: size, height: size)
        .clipped()
    }
}

/// The default picture for a person: a piece of the Kith sky (a cloud crop over a dawn, clear or dusk tone,
/// seeded by the account id). A picture, not a label: no letter.
public struct SkyPatch: View {
    let id: String
    let size: CGFloat
    let isYou: Bool
    var preset: SkyPreset?

    public init(id: String, size: CGFloat = KithSize.botAvatar, isYou: Bool = false) {
        self.id = id
        self.size = size
        self.isYou = isYou
    }

    public init(preset: SkyPreset, size: CGFloat = KithSize.botAvatar) {
        self.id = preset.id
        self.size = size
        self.isYou = false
        self.preset = preset
    }

    public var body: some View {
        SkyArt(tone: preset?.tone ?? SkyTone.tone(for: id, isYou: isYou),
               crop: preset?.crop ?? SkyTone.crop(for: id), size: size)
        .clipShape(Circle())
        .overlay(Circle().strokeBorder(Color.white.opacity(0.85), lineWidth: size >= 40 ? 1.5 : 1))
        .kithBotShadow()
        .accessibilityHidden(true)
    }
}

#Preview("Bot avatar states") {
    HStack(spacing: 18) {
        BotAvatar(portrait: "crew-coder", id: "coder", state: .idle)
        BotAvatar(portrait: "crew-coder", id: "coder", state: .working)
        BotAvatar(portrait: "crew-designer", id: "designer", state: .needsYou(2))
        BotAvatar(portrait: "crew-builder", id: "builder", state: .asleep)
    }
    .padding(30)
}

#Preview("Sizes and sky") {
    HStack(spacing: 14) {
        BotAvatar(portrait: "crew-house", id: "house", size: KithSize.botAvatarColourSheet)
        BotAvatar(portrait: "crew-house", id: "house", size: KithSize.botAvatar)
        BotAvatar(portrait: "crew-house", id: "house", size: KithSize.botAvatarTopBar)
        BotAvatar(portrait: "crew-house", id: "house", size: KithSize.botAvatarThread)
        SkyPatch(id: "kai", size: 48, isYou: true)
        SkyPatch(id: "mia", size: 48)
        SkyPatch(id: "ren", size: 48)
    }
    .padding(30)
}
