import SwiftUI

/// A crew bot: a body drawn from art, two black ovals for eyes and, at 16 pt and up, motion
/// (MOTION.md). `portrait` is a `CrewAvatar` asset (`crew-lab`, `spare-chart`); nil is a group's
/// cube on its tile. `color` is the stored `avatarColor`: only `kith:<paletteId>` recolours.
public struct CharacterAvatar: View {
    public enum Mood: Sendable { case idle, working, needsInput, error }

    let portrait: String?
    let color: String
    let size: CGFloat
    let mood: Mood
    let still: Bool
    let botId: String

    public init(portrait: String?, color: String = "", size: CGFloat = 40, mood: Mood = .idle, still: Bool = false, id: String = "") {
        self.portrait = portrait
        self.color = color
        self.size = size
        self.mood = mood
        self.still = still
        self.botId = id
    }

    /// `animated: false` keeps the bot's blink but not its mood (lists, tiny faces).
    public init(bot: Bot, size: CGFloat = 40, animated: Bool = true) {
        self.init(portrait: CrewAvatar.asset(for: bot), color: bot.avatarColor, size: size, mood: animated ? bot.mood : .idle, id: bot.id)
    }

    /// The bot's current mood as one frame: widgets, notifications and Live Activities cannot run animation.
    public init(bot: Bot, size: CGFloat = 40, still: Bool) {
        self.init(portrait: CrewAvatar.asset(for: bot), color: bot.avatarColor, size: size, mood: bot.mood, still: still, id: bot.id)
    }

    public var body: some View {
        Group {
            if let portrait {
                BotFace(asset: portrait, color: color, botId: botId.isEmpty ? portrait : botId, size: size, mood: mood, still: still)
            } else {
                GroupAvatar(members: [], size: size, still: still)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

public extension Bot {
    var mood: CharacterAvatar.Mood { status == "error" ? .error : needsInput ? .needsInput : isWorking ? .working : .idle }
}

/// Body and eyes, as Core Animation when it moves and as plain SwiftUI when it does not
/// (a widget frame, under 16 pt, Reduce Motion), so the two always put the eyes in the same place.
struct BotFace: View {
    let asset: String
    var color = ""
    var botId: String
    let size: CGFloat
    var mood: CharacterAvatar.Mood = .idle
    var still = false
    var placement: BotPlacement = .identity
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let tier = AvatarTier.tier(size: size, still: still, reduceMotion: reduceMotion)
        let paletteId = LivingBotTint.paletteId(color)
        if tier == .still || tier == .reduced {
            StillBot(asset: asset, paletteId: paletteId, size: size, mood: mood, placement: placement, reduced: tier == .reduced)
        } else {
            LivingBotRepresentable(spec: .init(asset: asset, paletteId: paletteId, botId: botId, size: size, mood: mood, placement: placement))
                .frame(width: size, height: size)
        }
    }
}

/// One frame of a bot: the state's eye pose and, for an error, the slump and droop. Reduce
/// Motion shows the eyes centred and nothing else.
struct StillBot: View {
    let asset: String
    let paletteId: String?
    let size: CGFloat
    let mood: CharacterAvatar.Mood
    var placement: BotPlacement = .identity
    var reduced = false

    var body: some View {
        if let rig = LivingBotData.rig(forAsset: asset), let image = LivingBotTint.image(asset: asset, paletteId: paletteId) {
            let g = BotGeometry(rig: rig, size: size, placement: placement)
            let box = g.body
            let pose = reduced ? (x: 0.0, y: 0.0) : BotPose.gaze(mood)
            let off = g.gazeOffset(pose.x, pose.y)
            let error = mood == .error && !reduced
            ZStack(alignment: .topLeading) {
                Image(decorative: image, scale: 1).resizable().interpolation(.medium).frame(width: box.width, height: box.height)
                ForEach(Array(rig.eyes.enumerated()), id: \.offset) { _, eye in
                    Ellipse().fill(.black)
                        .frame(width: eye.w * box.width, height: eye.h * box.width)
                        .rotationEffect(.degrees(eye.tilt))
                        .scaleEffect(x: 1, y: error ? BotPose.droop : 1)
                        .position(x: eye.x * box.width + off.width, y: eye.y * box.width + off.height)
                }
            }
            .frame(width: box.width, height: box.height, alignment: .topLeading)
            .scaleEffect(x: error ? BotPose.slump.sx : 1, y: error ? BotPose.slump.sy : 1,
                         anchor: UnitPoint(x: rig.anchor.x, y: rig.anchor.y))
            .position(x: box.midX, y: box.midY)
            .frame(width: size, height: size)
        } else {
            Color.clear.frame(width: size, height: size)
        }
    }
}

/// A group chat's face: the cube bot that holds the crew, with no tile behind it. Its eyes are drawn like any bot's, so the group blinks too.
public struct GroupAvatar: View {
    let size: CGFloat
    let still: Bool
    let id: String

    /// `members` is kept for the call sites; the tile shows the same cube whoever is in the chat.
    public init(members: [Bot] = [], size: CGFloat = 40, animated: Bool = true, still: Bool = false, id: String = "group") {
        self.size = size
        self.still = still
        self.id = id
    }

    public var body: some View {
        let placement = LivingBotData.rigs["kith"]?.group ?? BotPlacement(scale: 1.134, dx: 0, dy: 0.0045)
        BotFace(asset: "kith-cube", botId: id, size: size, still: still, placement: placement)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

/// Avatar with the roster status dot: lime = unread, orange = needs you.
/// A group shows its `members`.
public struct AvatarWithStatus: View {
    let bot: Bot
    let members: [Bot]
    let size: CGFloat

    public init(bot: Bot, members: [Bot] = [], size: CGFloat = 44) {
        self.bot = bot
        self.members = members
        self.size = size
    }

    public var body: some View {
        Group {
            if bot.isGroup { GroupAvatar(members: members, size: size, id: bot.id) } else { CharacterAvatar(bot: bot, size: size) }
        }
            .overlay(alignment: .bottomTrailing) {
                if bot.needsInput {
                    Image(systemName: "exclamationmark")
                        .font(.system(size: size * 0.2, weight: .black))
                        .foregroundStyle(.white)
                        .frame(width: size * 0.36, height: size * 0.36)
                        .background(Palette.warning, in: Circle())
                        .overlay(Circle().stroke(Palette.background, lineWidth: 2))
                } else if bot.unread > 0 {
                    Circle()
                        .fill(Palette.accentFill)
                        .frame(width: size * 0.28, height: size * 0.28)
                        .overlay(Circle().stroke(Palette.background, lineWidth: 2))
                }
            }
    }
}
