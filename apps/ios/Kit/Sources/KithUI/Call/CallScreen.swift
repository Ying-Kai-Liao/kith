import AVFoundation
import AVKit
import KithKit
import SwiftUI
import UIKit

/// A voice call with a bot, full screen over its chat: the bot's living avatar, what is being
/// said, and the call's buttons. What you say goes out as ordinary messages, its final replies
/// are read aloud, and the chat gets a "Voice chat" line when the call connects, with its length ("· 00:16") filled in at the end.
struct CallScreen: View {
    let botId: String
    let close: () -> Void
    /// An agent's call the person accepted: once the call is live the agent speaks first.
    var greet: String?
    /// Demo shots only: a fixed engine in place of a real one (used when `DemoScreens.active`).
    var engine: (any VoiceEngine)?
    @Environment(BotStore.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var session: (any VoiceEngine)?
    @State private var callID = UUID()
    @State private var startedAt = Date.now
    /// The chat's "Voice chat" line, made once the call connects (nil until then, or never).
    @State private var record: Task<String?, Never>?
    /// When the current cloud engine started, for this month's minutes.
    @State private var cloudStartedAt: Date?
    /// One line under the captions, e.g. why the call fell back to on-device voice.
    @State private var notice: String?
    @State private var settings = false
    @State private var speakerOn = true
    @State private var routeTick = 0
    @Environment(\.colorScheme) private var scheme

    private var bot: Bot? { model.bots[botId] }
    private var color: Color { bot.map { AvatarPalette.color($0.avatarColor) } ?? Palette.accent }
    private var speaking: Bool { session?.phase == .speaking }
    private var statusText: String {
        CallStatus.text(phase: session?.phase, muted: session?.muted == true,
                        working: bot?.isWorking == true, warning: session?.warning)
    }
    /// Mono top line: where the voice runs, and the computer the bot lives on.
    private var topLine: String { "\(session?.provider.map { "Voice by \($0.name)" } ?? "On device") · \(model.hostName)" }
    /// The glow rings the bot only while someone can be heard.
    private var ringShown: Bool { session?.phase.isActive == true && session?.muted != true }
    private var glowPair: (Color, Color)? {
        guard let id = AvatarPalette.pick(bot?.avatarColor), let swatch = AvatarPalette.colors.first(where: { $0.id == id }) else { return nil }
        return (Color(hex: swatch.a), Color(hex: swatch.b))
    }

    /// The one notice under the timer: why the call isn't clean, and Try again when the computer is gone.
    private var noticeInfo: (text: String, retry: Bool)? {
        if case .failed(let message) = session?.phase { return (message, false) }
        if let notice { return (notice, false) }
        if model.isOffline { return ("\(model.hostName) is unreachable.", true) }
        if let warning = session?.warning, session?.phase != .starting { return (warning, false) }
        return nil
    }

    var body: some View {
        VStack(spacing: 0) {
            Text(topLine)
                .font(Self.mono(12))
                .foregroundStyle(Palette.muted)
                .padding(.top, 8)
            Spacer(minLength: 16)
            avatar
            VStack(spacing: 4) {
                Text(bot?.name ?? "")
                    .kithText(.agentName)
                    .foregroundStyle(Palette.ink)
                Text(statusText)
                    .font(.custom(KithFonts.interRegular, size: 14))
                    .foregroundStyle(Palette.muted)
                TimelineView(.periodic(from: startedAt, by: 1)) { context in
                    Text(CallStatus.clock(Int(context.date.timeIntervalSince(startedAt))))
                        .font(Self.mono(12).monospacedDigit())
                        .foregroundStyle(Palette.ink)
                }
            }
            .padding(.top, 8)
            noticeBlock
                .padding(.top, 12)
            captions
                .padding(.top, 12)
            controls
                .padding(.bottom, 34)
        }
        .padding(.horizontal, 20)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background { CallSky().ignoresSafeArea() }
        .animation(Motion.reduced(Motion.fade, reduceMotion), value: session?.phase)
        .animation(Motion.reduced(Motion.fade, reduceMotion), value: noticeInfo?.text)
        .onAppear {
            startedAt = .now
            CallPresence.active += 1
            Dictation.stopRecording()
            Analytics.shared.capture(.voiceCallStarted)
            begin(cloud: VoiceSettings.provider)
        }
        .onDisappear {
            CallPresence.active -= 1
            session?.end()
            countCloudMinutes()
            model.endVoiceCall(callID)
            model.finishCall(botId, started: record, seconds: Int(Date.now.timeIntervalSince(startedAt)))
        }
        .onChange(of: session?.phase) { _, phase in
            // The line is written once the call really connects, so a call that never does leaves none.
            if phase?.isActive == true, record == nil { record = model.startCall(botId) }
            // A cloud engine that can't start or drops mid-call hands over to on-device voice.
            if case .failed(let message) = phase, let provider = session?.provider {
                countCloudMinutes()
                notice = "\(provider.name) isn't available (\(message)). Using on-device voice."
                begin(cloud: nil)
            }
        }
        .onChange(of: bot == nil) { _, gone in
            // The bot was deleted (on another device) mid-call.
            if gone { close() }
        }
        .kithSheet(isPresented: $settings) { VoiceChatSettingsView() }
    }

    /// Starts the engine: a realtime provider on the key kept by the computer, or on-device speech.
    private func begin(cloud provider: VoiceProvider?) {
        // Picks up the computer's newest models for the next call; this one starts right away.
        if provider != nil, let client = model.client {
            Task { if let status = try? await client.voiceStatus() { VoiceSettings.remember(status) } }
        }
        if let engine, DemoScreens.active { session = engine; return }
        let op = CallOperator(model: model, botId: botId)
        let engine: any VoiceEngine
        if let provider, VoiceSettings.isSpeechMode(provider) {
            let transcribe = VoiceSettings.transcribeModel(provider)
            let speechModel = VoiceSettings.speechModel(provider)
            let voice = VoiceSettings.voice(provider)
            let client = { [model] () throws -> HostClient in
                guard let client = model.client else { throw VoiceError("Not connected to \(model.hostName).") }
                return client
            }
            engine = CloudSpeechEngine(provider: provider, speech: .init(
                transcribe: { try await VoiceSettings.inReadersScript(client().voiceTranscribe(provider, model: transcribe, wav: $0)) },
                speak: { try await client().voiceSpeak(provider, model: speechModel, voice: voice, text: $0) },
                send: { [model, botId] in model.sendSpoken($0, to: botId) }
            ))
            cloudStartedAt = .now
        } else if let provider {
            let chosen = VoiceSettings.model(provider)
            let voice = VoiceSettings.voice(provider)
            let credential: @MainActor () async throws -> String = { [model] in
                guard let client = model.client else { throw VoiceError("Not connected to \(model.hostName).") }
                return try await client.voiceSession(provider, model: chosen, voice: voice)
            }
            engine = switch provider {
            case .openAI: OpenAIRealtimeEngine(credential: credential, op: op)
            case .gemini: GeminiLiveEngine(model: chosen, voice: voice, credential: credential, op: op)
            }
            cloudStartedAt = .now
        } else if VoiceSettings.isKith {
            // Apple speech to listen; Phonon on the computer to speak.
            let client = { [model] () throws -> HostClient in
                guard let client = model.client else { throw VoiceError("Not connected to \(model.hostName).") }
                return client
            }
            engine = KithVoiceEngine(host: .init(
                status: { try await client().voiceLocalStatus() },
                prepare: { try await client().voiceLocalPrepare() },
                speak: { try await client().voiceLocalSpeak($0, voice: $1) },
                voice: { VoiceSettings.kithVoice(in: $0) }
            )) { [model, botId] in model.sendSpoken($0, to: botId) }
        } else {
            engine = OnDeviceEngine { [model, botId] in model.sendSpoken($0, to: botId) }
        }
        let streams = engine.streamsReplies
        engine.onActivityChanged = { [weak engine, model, botId, callID, greet] (active: Bool) in
            guard let engine else { return }
            if active, let greet { model.greetCall(greet) }
            Self.setActivity(active, engine: engine, streams: streams, model: model, botId: botId, callID: callID)
        }
        session = engine
        Task { await engine.start() }
    }

    /// Audio is active: the store hands this call the bot's replies; otherwise it lets go of it.
    private static func setActivity(_ active: Bool, engine: any VoiceEngine, streams: Bool, model: BotStore, botId: String, callID: UUID) {
        guard active else { return model.endVoiceCall(callID) }
        var stream: (@MainActor (String, String, Bool) -> Void)?
        var working: (@MainActor (Bool) -> Void)?
        if streams {
            stream = { [weak engine] id, text, isFinal in engine?.streamReply(id: id, text: text, final: isFinal) }
            working = { [weak engine] busy in engine?.setWorking(busy) }
        }
        model.beginVoiceCall(callID, botId: botId,
                             speak: { [weak engine] in engine?.speak(reply: $0) },
                             announce: { [weak engine] in engine?.announce($0) },
                             stream: stream, working: working,
                             end: { [weak engine] in engine?.end() })
        engine.setWorking(model.bots[botId]?.isWorking ?? false)
    }

    private func countCloudMinutes() {
        guard let provider = session?.provider, let since = cloudStartedAt else { return }
        VoiceUsage.add(seconds: Int(Date.now.timeIntervalSince(since)), for: provider)
        cloudStartedAt = nil
    }

    private var noticeBlock: some View {
        Group {
            if let info = noticeInfo {
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: "info.circle")
                        .font(.system(size: 14))
                        .foregroundStyle(Palette.muted)
                        .padding(.top, 1)
                    Text(info.text)
                        .font(.custom(KithFonts.interRegular, size: 13))
                        .foregroundStyle(Palette.muted)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                    if info.retry {
                        Button("Try again") { retry() }
                            .font(.custom(KithFonts.interSemiBold, size: 13))
                            .foregroundStyle(Palette.accent)
                    }
                }
                .padding(12)
                .background(Palette.panel, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                .transition(.opacity)
            }
        }
    }

    /// Starts the engine again (the computer was unreachable): the old one is let go first.
    private func retry() {
        session?.end()
        model.endVoiceCall(callID)
        notice = nil
        begin(cloud: VoiceSettings.provider)
    }

    /// The bot at 150 pt in a glow ring that swells with the voice level; tapping it while it talks
    /// cuts the reply short.
    private var avatar: some View {
        let level = CGFloat(session?.level ?? 0)
        return Button { session?.interrupt() } label: {
            ZStack {
                if ringShown {
                    CallGlow(pair: glowPair)
                        .frame(width: 200, height: 200)
                        .scaleEffect(reduceMotion ? 1 : 1 + level * 0.2 + (speaking ? 0.04 : 0))
                        .opacity(0.55 + Double(level) * 0.45)
                        .animation(reduceMotion ? nil : .easeOut(duration: 0.12), value: level)
                        .transition(.opacity)
                }
                if let bot { CharacterAvatar(bot: bot, size: 160).kithBotShadow() }
            }
            .frame(width: 220, height: 220)
        }
        .buttonStyle(.plain)
        .disabled(!speaking)
        .accessibilityLabel(speaking ? "Interrupt" : bot?.name ?? "")
    }

    /// What you said and what the bot is saying, newest at the bottom; your words still being
    /// heard show dimmed under them.
    private var captions: some View {
        let lines = session?.captions ?? []
        let heard = session?.heard ?? ""
        return ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(Array(lines.enumerated()), id: \.element.id) { index, caption in
                        captionLine(caption.speaker == .you ? "You" : (bot?.name ?? ""), caption.text,
                                    newest: heard.isEmpty && index == lines.count - 1)
                            .id(caption.id)
                    }
                    if !heard.isEmpty {
                        captionLine("You", heard, newest: true).id("heard")
                    }
                }
                .padding(.vertical, 8)
            }
            .scrollIndicators(.hidden)
            .onChange(of: lines.last?.text) { _, _ in scroll(proxy, lines: lines, heard: heard) }
            .onChange(of: heard) { _, _ in scroll(proxy, lines: lines, heard: heard) }
        }
        .frame(maxHeight: .infinity)
        .accessibilityLabel("Captions")
    }

    /// A caption: the speaker in bold, then the words; the newest in ink, older ones muted.
    private func captionLine(_ speaker: String, _ text: String, newest: Bool) -> some View {
        (Text(speaker).fontWeight(.bold) + Text("  " + text))
            .font(.custom(KithFonts.interRegular, size: 15))
            .foregroundStyle(newest ? Palette.ink : Palette.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func scroll(_ proxy: ScrollViewProxy, lines: [CallCaption], heard: String) {
        let target: AnyHashable? = heard.isEmpty ? lines.last?.id : "heard"
        guard let target else { return }
        withAnimation(Motion.reduced(.easeOut(duration: 0.2), reduceMotion)) { proxy.scrollTo(target, anchor: .bottom) }
    }

    private var controls: some View {
        let muted = session?.muted == true
        let dark = scheme == .dark
        return HStack(alignment: .top, spacing: 12) {
            control(speakerOn ? "Speaker" : "Earpiece") {
                Image(systemName: speakerOn ? "speaker.wave.3.fill" : "ear")
                    .font(.system(size: 22, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .contentTransition(.symbolEffect(.replace))
                    .frame(width: Self.buttonSize, height: Self.buttonSize)
                    .kithCallGlass(dark: dark)
                    .contentShape(Circle())
                    .onTapGesture {
                        guard session?.supportsSpeaker == true else { return }
                        speakerOn.toggle()
                        session?.setSpeaker(speakerOn)
                    }
                    // Bluetooth and AirPlay outputs, from the system.
                    .onLongPressGesture(minimumDuration: 0.4) { routeTick += 1 }
                    .overlay { RoutePicker(trigger: routeTick).frame(width: 1, height: 1).allowsHitTesting(false) }
                    .accessibilityElement()
                    .accessibilityLabel(speakerOn ? "Switch to earpiece" : "Switch to speaker")
                    .accessibilityAddTraits(.isButton)
                    .accessibilityAction(named: "Audio output") { routeTick += 1 }
            }
            control(muted ? "Unmute" : "Mute") {
                Button { session?.muted.toggle() } label: {
                    Image(systemName: muted ? "mic.slash.fill" : "mic.fill")
                        .font(.system(size: 22, weight: .medium))
                        .foregroundStyle(muted ? (dark ? Palette.night : Color.white) : Palette.ink)
                        .contentTransition(.symbolEffect(.replace))
                        .frame(width: Self.buttonSize, height: Self.buttonSize)
                        .background { if muted { Circle().fill(dark ? Color.white : Palette.ink) } }
                        .kithCallGlass(dark: dark && !muted)
                }
                .buttonStyle(PressScale())
                .accessibilityLabel(muted ? "Unmute" : "Mute")
            }
            control("Settings") {
                Button { settings = true } label: {
                    Image(systemName: "slider.horizontal.3")
                        .font(.system(size: 22, weight: .medium))
                        .foregroundStyle(Palette.ink)
                        .frame(width: Self.buttonSize, height: Self.buttonSize)
                        .kithCallGlass(dark: dark)
                }
                .buttonStyle(PressScale())
                .accessibilityLabel("Call settings")
            }
            control("End") {
                Button(action: close) {
                    Image(systemName: "phone.down.fill")
                        .font(.system(size: 22, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(width: Self.buttonSize, height: Self.buttonSize)
                        .background(Color(hex: 0xFF3B30), in: Circle())
                }
                .buttonStyle(PressScale())
                .accessibilityLabel("End call")
            }
        }
    }

    /// A button with its label under it.
    private func control<Content: View>(_ label: String, @ViewBuilder _ content: () -> Content) -> some View {
        VStack(spacing: 8) {
            content()
            Text(label)
                .font(.custom(KithFonts.interRegular, size: 12))
                .foregroundStyle(Palette.muted)
        }
        .frame(maxWidth: .infinity)
    }

    static func mono(_ size: CGFloat) -> Font {
        .custom(KithFonts.jetBrainsMono, size: size)
    }

    private static let buttonSize: CGFloat = 62
}

extension View {
    /// A round glass call button; in dark a local coal tint at 62% keeps it legible over the sky.
    func kithCallGlass(dark: Bool) -> some View {
        background { if dark { Circle().fill(Palette.night.opacity(0.62)) } }
            .kithGlass(in: Circle(), interactive: true)
    }
}

/// The sky behind a call: morning by day, night-1 in dark, fading into the ground from 35% to 78% down.
struct CallSky: View {
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        ZStack(alignment: .top) {
            Palette.background
            GeometryReader { proxy in
                Image(scheme == .dark ? "sky-home-night" : "sky-home-day", bundle: .module)
                    .resizable()
                    .scaledToFill()
                    .frame(width: proxy.size.width, height: proxy.size.height, alignment: .top)
                    .clipped()
                    .mask(LinearGradient(stops: [.init(color: .black, location: 0.35), .init(color: .clear, location: 0.78)],
                                         startPoint: .top, endPoint: .bottom))
            }
        }
        .accessibilityHidden(true)
    }
}

/// The radial glow behind a bot in a call, in the agent's colour pair (a neutral sky glow without one).
struct CallGlow: View {
    let pair: (Color, Color)?

    var body: some View {
        let (a, b) = pair ?? (Color(hex: 0x8FB0FF), Color(hex: 0xBFE3FF))
        Circle()
            .fill(RadialGradient(colors: [b.opacity(0.9), a.opacity(0.55), a.opacity(0)], center: .center, startRadius: 40, endRadius: 100))
    }
}

/// The system's output picker (AirPods, Bluetooth speakers, AirPlay). It is invisible; a long press on
/// Speaker bumps `trigger`, which presses the picker's own button.
private struct RoutePicker: UIViewRepresentable {
    let trigger: Int

    final class Coordinator { var last = 0 }
    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> AVRoutePickerView {
        let picker = AVRoutePickerView()
        picker.alpha = 0.011
        return picker
    }

    func updateUIView(_ view: AVRoutePickerView, context: Context) {
        guard trigger != context.coordinator.last else { return }
        context.coordinator.last = trigger
        (view.subviews.first { $0 is UIButton } as? UIButton)?.sendActions(for: .touchUpInside)
    }
}
