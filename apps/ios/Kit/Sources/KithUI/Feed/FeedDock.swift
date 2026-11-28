import KithKit
import SwiftUI

/// What the app target wires into the Feed's box so + and the call button work as in Crew: the pickers, the call, and
/// the staged files. KithUI stays free of the app's pickers; the files go out after the text, only once it went.
public struct FeedAttach {
    public var hasFiles: Bool
    /// The staged files' chips, drawn by the app.
    public var strip: AnyView
    public var onCall: () -> Void
    public var onPhotos: () -> Void
    public var onCamera: () -> Void
    public var onFiles: () -> Void
    /// The text went to this chat: send the staged files there.
    public var onSent: (_ chat: String) -> Void

    public init(hasFiles: Bool, strip: AnyView, onCall: @escaping () -> Void, onPhotos: @escaping () -> Void,
                onCamera: @escaping () -> Void, onFiles: @escaping () -> Void, onSent: @escaping (_ chat: String) -> Void) {
        self.hasFiles = hasFiles
        self.strip = strip
        self.onCall = onCall
        self.onPhotos = onPhotos
        self.onCamera = onCamera
        self.onFiles = onFiles
        self.onSent = onSent
    }
}

/// The Feed's bottom box: Crew's own `ChatDock`, with the Feed's routing in its accessory slot. @ chips choose the
/// agent; with none, the computer's pick shows as a chip to change. Nothing is sent on a pick the person has not seen.
struct FeedDock: View {
    let composer: FeedComposer
    let onOpenChat: (_ chat: String, _ entry: String?) -> Void
    let attach: FeedAttach?
    @State private var dictation = HomeDictation()

    var body: some View {
        ChatDock(text: Binding(get: { composer.text }, set: { composer.edit($0) }), placeholder: "Ask anyone",
                 hasAttachment: attach?.hasFiles ?? false,
                 onSend: send,
                 onMic: { dictation.toggle(current: composer.text) { composer.edit($0) } },
                 onCall: { dictation.stop(); attach?.onCall() },
                 onPhotos: { attach?.onPhotos() }, onCamera: { attach?.onCamera() }, onFiles: { attach?.onFiles() },
                 sendStyle: composer.canSend ? .ink : .muted, micListening: dictation.listening,
                 canSend: composer.canSend) {
            accessory
        }
        .padding(.bottom, 4)
        // Swallow taps in the dock's gaps so they never reach a post scrolling beneath it.
        .background(Color.clear.contentShape(Rectangle()).onTapGesture {})
        .onDisappear { dictation.stop() }
    }

    private func send() {
        dictation.stop()
        Task {
            await composer.send()
            // The files follow the text only when it went, to the chat it went to: a pick never seen sends nothing.
            if let sent = composer.sent { attach?.onSent(sent.chat) }
        }
    }

    private var accessory: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let sent = composer.sent {
                HStack {
                    Text("Sent to \(composer.name(of: sent.agent))").font(KithText.secondary.font).foregroundStyle(Palette.muted)
                    Button("Open") { onOpenChat(sent.chat, nil) }.font(KithText.secondary.font).foregroundStyle(Palette.accent)
                }
            }
            if let error = composer.error { Text(error).font(KithText.secondary.font).foregroundStyle(Palette.negative) }
            if let attach, attach.hasFiles { attach.strip }
            chips
        }
        .padding(.horizontal, 14).padding(.top, 10)
    }

    /// The chosen or picked agent first and filled; the rest as outlines.
    private var chips: some View {
        let picked = composer.target
        return ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 6) {
                if composer.tag == nil, !composer.trimmed.isEmpty {
                    if composer.routing && picked == nil {
                        Text("Choosing…").font(KithText.label.font).foregroundStyle(Palette.muted).padding(.horizontal, 6)
                    }
                }
                ForEach(ordered(picked)) { agent in
                    Button { composer.choose(agent.id) } label: {
                        Text("@\(agent.name)").font(KithText.label.font)
                            .padding(.horizontal, 10).padding(.vertical, 5)
                            .foregroundStyle(agent.id == picked ? Palette.onAccent : Palette.accent)
                            .background(agent.id == picked ? Palette.accentFill : Palette.accentDim, in: Capsule())
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(agent.id == picked ? .isSelected : [])
                }
            }
        }
    }

    private func ordered(_ picked: String?) -> [FeedComposer.Agent] {
        let all = composer.agents
        guard let picked, let i = all.firstIndex(where: { $0.id == picked }) else { return all }
        return [all[i]] + all.enumerated().filter { $0.offset != i }.map(\.element)
    }
}
