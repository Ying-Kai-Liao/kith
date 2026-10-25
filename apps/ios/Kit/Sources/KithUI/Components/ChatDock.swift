import KithKit
import SwiftUI

/// What the dock shows for a given state. Pure, so it is tested.
public enum ChatDockLayout {
    /// Anything typed or attached counts as "writing".
    public static func isWriting(text: String, hasAttachment: Bool) -> Bool {
        hasAttachment || !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// The call button sits to the right of the box; it slides away while writing and comes back when cleared.
    /// `showsCall` is false where the call lives elsewhere (the agent chat has the phone at the top).
    public static func callVisible(showsCall: Bool, text: String, hasAttachment: Bool) -> Bool {
        showsCall && !isWriting(text: text, hasAttachment: hasAttachment)
    }

    /// Send arrow when there is something to send, mic otherwise.
    public static func showsSend(text: String, hasAttachment: Bool) -> Bool {
        isWriting(text: text, hasAttachment: hasAttachment)
    }

    public enum Trailing: Hashable, Sendable {
        case mic, send, stop, stopRecording
    }

    /// The buttons after the field, left to right. While recording: its Stop and Send. Otherwise Send while writing
    /// (after the mic when `micWithText`), the screen's Stop beside the mic while the box is empty, or the mic alone.
    public static func trailing(text: String, hasAttachment: Bool, recording: Bool, micWithText: Bool, hasStop: Bool) -> [Trailing] {
        if recording { return [.stopRecording, .send] }
        if showsSend(text: text, hasAttachment: hasAttachment) { return micWithText ? [.mic, .send] : [.send] }
        return hasStop ? [.mic, .stop] : [.mic]
    }
}

/// The dock's Stop beside the mic while the box is empty: the bot's work, or its voice in a call.
public struct ChatDockStop {
    public let title: String
    /// Red, for cutting a voice off; ink stops work.
    public let urgent: Bool
    public let action: () -> Void

    public init(_ title: String, urgent: Bool = false, action: @escaping () -> Void) {
        self.title = title
        self.urgent = urgent
        self.action = action
    }
}

/// One more item for the `+` menu, after Photos / Camera / Files (the chat adds Paste and New task).
public struct ChatDockAction: Identifiable {
    public let id: String
    public let title: String
    public let systemImage: String
    public let action: () -> Void

    public init(_ title: String, systemImage: String, action: @escaping () -> Void) {
        id = title
        self.title = title
        self.systemImage = systemImage
        self.action = action
    }
}

/// How the send button looks. Additive: Crew home greys the arrow while the typed words lead to a room and spells
/// out "Send" when nothing matches; chats keep `.ink`.
public enum ChatDockSendStyle: Sendable {
    case ink, muted, word
}

/// The chat box: a floating Liquid Glass pill (54 pt). `+` opens Photos / Camera / Files; a text field; mic when
/// empty, send when there is text; a round glass call button to its right that slides away while typing. While
/// dictation records, the field is read-only and the mic gives way to Stop (keep the words to edit) and Send.
///
/// Binding-based, with no sending logic: the screen owns the text, sends, queues and dictates. `accessory` is a
/// slot above the field inside the pill for the composer's attachment strip, command list or router line later.
public struct ChatDock<Accessory: View>: View {
    @Binding var text: String
    let placeholder: String
    let showsCall: Bool
    let hasAttachment: Bool
    let onSend: () -> Void
    let onMic: () -> Void
    let onCall: () -> Void
    let onPhotos: () -> Void
    let onCamera: () -> Void
    let onFiles: () -> Void
    let sendStyle: ChatDockSendStyle
    let extraActions: [ChatDockAction]
    /// Dictation is recording: Stop (`onMic`) and Send replace the mic, and the field can't be edited.
    let micListening: Bool
    /// The chat keeps the mic beside Send while there is text, so dictation can add to a draft.
    let micWithText: Bool
    /// False greys Send (offline and the message can't wait in the relay).
    let canSend: Bool
    let showsPlus: Bool
    let stop: ChatDockStop?
    let focus: FocusState<Bool>.Binding?
    let accessory: Accessory
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    public init(text: Binding<String>, placeholder: String, showsCall: Bool = true, hasAttachment: Bool = false,
                onSend: @escaping () -> Void = {}, onMic: @escaping () -> Void = {}, onCall: @escaping () -> Void = {},
                onPhotos: @escaping () -> Void = {}, onCamera: @escaping () -> Void = {}, onFiles: @escaping () -> Void = {},
                sendStyle: ChatDockSendStyle = .ink, extraActions: [ChatDockAction] = [], micListening: Bool = false,
                micWithText: Bool = false, canSend: Bool = true, showsPlus: Bool = true, stop: ChatDockStop? = nil,
                focus: FocusState<Bool>.Binding? = nil, @ViewBuilder accessory: () -> Accessory) {
        _text = text
        self.placeholder = placeholder
        self.showsCall = showsCall
        self.hasAttachment = hasAttachment
        self.onSend = onSend
        self.onMic = onMic
        self.onCall = onCall
        self.onPhotos = onPhotos
        self.onCamera = onCamera
        self.onFiles = onFiles
        self.sendStyle = sendStyle
        self.extraActions = extraActions
        self.micListening = micListening
        self.micWithText = micWithText
        self.canSend = canSend
        self.showsPlus = showsPlus
        self.stop = stop
        self.focus = focus
        self.accessory = accessory()
    }

    private var callSize: CGFloat { max(KithSize.chatBoxHeight, 54) }

    private var callVisible: Bool { ChatDockLayout.callVisible(showsCall: showsCall, text: text, hasAttachment: hasAttachment) }

    public var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            VStack(spacing: 0) {
                accessory
                HStack(spacing: 4) {
                    if showsPlus { plusMenu }
                    TextField(placeholder, text: $text, axis: .vertical)
                        .kithText(.body)
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1...5)
                        .padding(.vertical, 8)
                        .disabled(micListening)
                        .modifier(OptionalFocus(binding: focus))
                    ForEach(trailing, id: \.self) { button in
                        switch button {
                        case .mic: micButton
                        case .send: sendButton
                        case .stop: if let stop { stopButton(stop) }
                        case .stopRecording: stopRecordingButton
                        }
                    }
                }
                .padding(.horizontal, 5)
                .frame(minHeight: KithSize.chatBoxHeight)
            }
            .kithGlassPill()
            if callVisible {
                Button(action: onCall) {
                    Image(systemName: "phone.fill").font(.system(size: 18, weight: .medium)).foregroundStyle(Palette.ink)
                        .frame(width: callSize, height: callSize)
                        .kithGlass(in: Circle(), interactive: true)
                        // The whole circle takes the tap, so it never falls through to the row below.
                        .contentShape(Circle())
                }
                .buttonStyle(.plain)
                .zIndex(1)
                .accessibilityLabel(Text("Call"))
                .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
            }
        }
        .padding(.horizontal, KithSpacing.gutter)
        .animation(reduceMotion ? nil : .spring(duration: 0.3, bounce: 0.1), value: callVisible)
    }

    private var plusMenu: some View {
        Menu {
            Button(action: onPhotos) { Label("Photos", systemImage: "photo") }
            Button(action: onCamera) { Label("Camera", systemImage: "camera") }
            Button(action: onFiles) { Label("Files", systemImage: "folder") }
            ForEach(extraActions) { extra in
                Button(action: extra.action) { Label(extra.title, systemImage: extra.systemImage) }
            }
        } label: {
            Image(systemName: "plus").font(.system(size: 18, weight: .medium)).foregroundStyle(Palette.ink)
                .frame(width: 44, height: 44)
                .background(Palette.ink.opacity(0.06), in: Circle())
                .contentShape(Circle())
        }
        .accessibilityLabel(Text("Add"))
    }

    private var trailing: [ChatDockLayout.Trailing] {
        ChatDockLayout.trailing(text: text, hasAttachment: hasAttachment, recording: micListening, micWithText: micWithText,
                                hasStop: stop != nil)
    }

    private var sendButton: some View {
            Button(action: onSend) {
                switch sendStyle {
                case .word:
                    Text("Send").kithText(.body).fontWeight(.semibold).foregroundStyle(Palette.background)
                        .padding(.horizontal, 16).frame(minWidth: 44, minHeight: 44)
                        .background(Palette.ink, in: Capsule())
                        .contentShape(Capsule())
                default:
                    Image(systemName: "arrow.up").font(.system(size: 16, weight: .bold)).foregroundStyle(Palette.background)
                        .frame(width: 44, height: 44)
                        .background(sendStyle == .muted ? Palette.muted : Palette.ink, in: Circle())
                        .contentShape(Circle())
                }
            }
            .buttonStyle(.plain)
            .disabled(!canSend)
            .opacity(canSend ? 1 : 0.4)
            .accessibilityLabel(Text("Send"))
    }

    private var micButton: some View {
            Button(action: onMic) {
                Image(systemName: "mic").font(.system(size: 18))
                    .foregroundStyle(Palette.muted)
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("Dictate"))
    }

    /// Ends the recording; the words stay in the box to edit.
    private var stopRecordingButton: some View {
        roundStop(tint: Palette.negative, label: "Stop recording", action: onMic)
    }

    private func stopButton(_ stop: ChatDockStop) -> some View {
        roundStop(tint: stop.urgent ? Palette.negative : Palette.ink, label: stop.title, action: stop.action)
    }

    private func roundStop(tint: Color, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: "stop.fill").font(.system(size: 14, weight: .bold)).foregroundStyle(tint)
                .frame(width: 44, height: 44)
                .background(tint.opacity(0.12), in: Circle())
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(label))
    }
}

private struct OptionalFocus: ViewModifier {
    let binding: FocusState<Bool>.Binding?

    func body(content: Content) -> some View {
        if let binding { content.focused(binding) } else { content }
    }
}

public extension ChatDock where Accessory == EmptyView {
    init(text: Binding<String>, placeholder: String, showsCall: Bool = true, hasAttachment: Bool = false,
         onSend: @escaping () -> Void = {}, onMic: @escaping () -> Void = {}, onCall: @escaping () -> Void = {},
         onPhotos: @escaping () -> Void = {}, onCamera: @escaping () -> Void = {}, onFiles: @escaping () -> Void = {},
         sendStyle: ChatDockSendStyle = .ink, extraActions: [ChatDockAction] = [], micListening: Bool = false,
         micWithText: Bool = false, canSend: Bool = true, showsPlus: Bool = true, stop: ChatDockStop? = nil,
         focus: FocusState<Bool>.Binding? = nil) {
        self.init(text: text, placeholder: placeholder, showsCall: showsCall, hasAttachment: hasAttachment, onSend: onSend,
                  onMic: onMic, onCall: onCall, onPhotos: onPhotos, onCamera: onCamera, onFiles: onFiles,
                  sendStyle: sendStyle, extraActions: extraActions, micListening: micListening, micWithText: micWithText,
                  canSend: canSend, showsPlus: showsPlus, stop: stop, focus: focus) { EmptyView() }
    }
}

#Preview("Dock, empty") {
    ZStack(alignment: .bottom) {
        SkyBackground(.home, edgeToEdge: false)
        ChatDock(text: .constant(""), placeholder: "Find or ask…")
    }
    .frame(height: 200)
}

#Preview("Dock, typing") {
    ZStack(alignment: .bottom) {
        SkyBackground(.home, edgeToEdge: false)
        ChatDock(text: .constant("Why is the mini so slow?"), placeholder: "Find or ask…")
    }
    .frame(height: 200)
}

#Preview("Dock, agent chat, attachment") {
    ZStack(alignment: .bottom) {
        SkyBackground(.header, edgeToEdge: false)
        ChatDock(text: .constant(""), placeholder: "Tell coder…", showsCall: false, hasAttachment: true) {
            HStack { Label("screenshot.png", systemImage: "photo").kithText(.secondary); Spacer() }
                .padding(.horizontal, 14).padding(.top, 10)
        }
    }
    .frame(height: 200)
}
