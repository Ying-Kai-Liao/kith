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

/// One of the eight Kith sky pictures a person can pick as their own: two palette colours behind a zoomed crop of
/// the sky art's clouds. Mirrors desktop's `SKY_PRESETS` (same order, same values).
public struct SkyPreset: Identifiable, Equatable, Sendable {
    public let name: String
    /// Palette, top to bottom, as 0xRRGGBB.
    let top: UInt32, bottom: UInt32
    /// How many picture-widths of sky art span the tile; higher is a tighter crop.
    let zoom: Int
    /// Crop position in percent of the movable range (CSS background-position).
    let x: Int, y: Int
    /// How strongly the clouds show over the palette, in percent.
    let cloud: Int
    public var id: String { name }

    public static let all: [SkyPreset] = [
        .init(name: "night", top: 0x050A24, bottom: 0x1B2F7A, zoom: 3, x: 0, y: 0, cloud: 30),
        .init(name: "golden", top: 0xE8801A, bottom: 0xFFD27A, zoom: 3, x: 100, y: 0, cloud: 55),
        .init(name: "dusk", top: 0x4A2F98, bottom: 0xF07A5A, zoom: 4, x: 90, y: 10, cloud: 50),
        .init(name: "dawn", top: 0x6FA0E8, bottom: 0xF8B890, zoom: 4, x: 5, y: 15, cloud: 55),
        .init(name: "noon", top: 0x0F4FC8, bottom: 0x4FA8F2, zoom: 3, x: 80, y: 40, cloud: 60),
        .init(name: "overcast", top: 0x58626E, bottom: 0xAEB6BE, zoom: 2, x: 40, y: 0, cloud: 75),
        .init(name: "twilight", top: 0x141C5E, bottom: 0xC8409A, zoom: 4, x: 10, y: 30, cloud: 45),
        .init(name: "mint", top: 0x1E8A78, bottom: 0xBDEBD0, zoom: 3, x: 85, y: 70, cloud: 60),
    ]

    /// A square picture of the preset, ready to upload.
    @MainActor public func renderedJPEG(side: CGFloat = 512) -> Data? {
        let renderer = ImageRenderer(content: SkyPresetArt(preset: self, size: side))
        renderer.scale = 1
        return renderer.uiImage?.jpegData(compressionQuality: 0.9)
    }
}

/// A preset's square picture: the palette gradient, then the cloud crop placed by the CSS background rules
/// (width = zoom x tile, offset = (tile - image) x position), so it matches desktop.
struct SkyPresetArt: View {
    let preset: SkyPreset
    let size: CGFloat

    var body: some View {
        let art = UIImage(named: "sky-header-day", in: KithUIFonts.resources, with: nil)
        let aspect = art.map { $0.size.height / $0.size.width } ?? 0.509
        let w = size * CGFloat(preset.zoom), h = w * aspect
        ZStack(alignment: .topLeading) {
            LinearGradient(colors: [Color(hex: preset.top), Color(hex: preset.bottom)], startPoint: .top, endPoint: .bottom)
            Image("sky-header-day", bundle: KithUIFonts.resources)
                .resizable()
                .frame(width: w, height: h)
                .offset(x: (size - w) * CGFloat(preset.x) / 100, y: (size - h) * CGFloat(preset.y) / 100)
                .opacity(Double(preset.cloud) / 100)
        }
        .frame(width: size, height: size, alignment: .topLeading)
        .clipped()
    }
}

/// The square sky picture behind `SkyPatch`.
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
        Group {
            if let preset {
                SkyPresetArt(preset: preset, size: size)
            } else {
                SkyArt(tone: SkyTone.tone(for: id, isYou: isYou), crop: SkyTone.crop(for: id), size: size)
            }
        }
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
