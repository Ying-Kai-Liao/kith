import KithKit
import SwiftUI

/// The Feed's bottom box: Crew's own `ChatDock`, with the Feed's routing in its accessory slot. @ chips choose the
/// agent; with none, the computer's pick shows as a chip to change. Nothing is sent on a pick the person has not seen.
struct FeedDock: View {
    let composer: FeedComposer
    let onOpenChat: (_ chat: String, _ entry: String?) -> Void
    @State private var dictation = HomeDictation()

    var body: some View {
        ChatDock(text: Binding(get: { composer.text }, set: { composer.edit($0) }), placeholder: "Ask anyone",
                 showsCall: false,
                 onSend: send,
                 onMic: { dictation.toggle(current: composer.text) { composer.edit($0) } },
                 sendStyle: composer.canSend ? .ink : .muted, micListening: dictation.listening,
                 canSend: composer.canSend, showsPlus: false) {
            accessory
        }
        .padding(.bottom, 4)
        // Swallow taps in the dock's gaps so they never reach a post scrolling beneath it.
        .background(Color.clear.contentShape(Rectangle()).onTapGesture {})
        .onDisappear { dictation.stop() }
    }

    private func send() {
        dictation.stop()
        Task { await composer.send() }
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
