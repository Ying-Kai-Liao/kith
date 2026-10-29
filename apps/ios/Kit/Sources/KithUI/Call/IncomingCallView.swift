import KithKit
import SwiftUI

/// An agent is calling: full screen with its face, name and why, Accept and Decline. It rings (and vibrates) while
/// it is up. There is no push, so it appears only in an open app; any device that answers, or the timeout, takes it
/// away from the others.
public struct IncomingCallView: View {
    let call: IncomingCall
    let bot: Bot?
    let accept: () -> Void
    let decline: () -> Void
    /// Declines, then opens the bot's chat.
    let reply: () -> Void
    /// Declines, then rings again in 10 minutes; nil hides the pill.
    let snooze: (() -> Void)?
    @State private var ring = RingTone()
    /// Set by the first tap on any of the three buttons; the others then do nothing.
    @State private var answered = false
    @State private var pulse = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    public init(call: IncomingCall, bot: Bot?, accept: @escaping () -> Void, decline: @escaping () -> Void,
                reply: @escaping () -> Void, snooze: (() -> Void)? = nil) {
        self.call = call
        self.bot = bot
        self.accept = accept
        self.decline = decline
        self.reply = reply
        self.snooze = snooze
    }

    private var name: String { bot?.name ?? call.name }
    private var glowPair: (Color, Color)? {
        guard let id = AvatarPalette.pick(bot?.avatarColor), let swatch = AvatarPalette.colors.first(where: { $0.id == id }) else { return nil }
        return (Color(hex: swatch.a), Color(hex: swatch.b))
    }
    /// The ring started 45 s before the deadline.
    private var time: String { call.deadline.addingTimeInterval(-45).formatted(date: .omitted, time: .shortened) }

    public var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 24)
            ZStack {
                CallGlow(pair: glowPair)
                    .frame(width: 200, height: 200)
                    .scaleEffect(pulse ? 1.08 : 0.96)
                    .opacity(pulse ? 1 : 0.7)
                if let bot { CharacterAvatar(bot: bot, size: 160).kithBotShadow() }
            }
            .frame(width: 220, height: 220)
            VStack(spacing: 4) {
                Text(name)
                    .kithText(.agentName)
                    .foregroundStyle(Palette.ink)
                Text("is calling you")
                    .font(.custom(KithFonts.interRegular, size: 14))
                    .foregroundStyle(Palette.muted)
            }
            .padding(.top, 8)
            if !call.reason.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text(call.reason)
                        .font(.custom(KithFonts.interRegular, size: 15))
                        .foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(time)
                        .font(.custom(KithFonts.interRegular, size: 12))
                        .foregroundStyle(Palette.muted)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
                .kithGlass(in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                .padding(.top, 20)
                .padding(.horizontal, 24)
            }
            Spacer()
            HStack(spacing: 12) {
                pill("Reply in chat", reply)
                if let snooze { pill("In 10 min", snooze) }
            }
            .padding(.bottom, 28)
            HStack(spacing: 72) {
                button("Decline", "phone.down.fill", Color(hex: 0xFF3B30)) {
                    guard !answered else { return }
                    answered = true
                    ring.stop()
                    decline()
                }
                button("Accept", "phone.fill", Color(hex: 0x34C759)) {
                    guard !answered else { return }
                    answered = true
                    // The call's own audio session takes over from the ring.
                    ring.stop()
                    accept()
                }
            }
            .padding(.bottom, 44)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background { CallSky().ignoresSafeArea() }
        .onAppear {
            ring.start()
            if !reduceMotion { withAnimation(.easeInOut(duration: 1.2).repeatForever(autoreverses: true)) { pulse = true } }
            else { pulse = true }
        }
        .onDisappear { ring.stop() }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(name) is calling")
    }

    private func pill(_ title: String, _ action: @escaping () -> Void) -> some View {
        Button {
            guard !answered else { return }
            answered = true
            ring.stop()
            action()
        } label: {
            Text(title)
                .font(.custom(KithFonts.interSemiBold, size: 15))
                .foregroundStyle(Palette.ink)
                .padding(.horizontal, 22)
                .padding(.vertical, 12)
                .kithGlass(in: Capsule(), interactive: true)
        }
        .buttonStyle(PressScale())
    }

    private func button(_ label: String, _ symbol: String, _ fill: Color, action: @escaping () -> Void) -> some View {
        VStack(spacing: 8) {
            Button(action: action) {
                Image(systemName: symbol)
                    .font(.system(size: 28, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 72, height: 72)
                    .background(fill, in: Circle())
            }
            .buttonStyle(PressScale())
            .accessibilityLabel(label)
            Text(label)
                .font(.custom(KithFonts.interRegular, size: 12))
                .foregroundStyle(Palette.muted)
        }
    }
}
