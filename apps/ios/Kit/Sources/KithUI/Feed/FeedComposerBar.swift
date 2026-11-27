import KithKit
import SwiftUI

/// The Feed's bottom box: @ chips choose the agent; with none, the computer's pick shows as a chip to change.
struct FeedComposerBar: View {
    let composer: FeedComposer
    let onOpenChat: (_ chat: String, _ entry: String?) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let sent = composer.sent {
                HStack {
                    Text("Sent to \(composer.name(of: sent.agent))").font(KithText.secondary.font).foregroundStyle(Palette.muted)
                    Button("Open") { onOpenChat(sent.chat, nil) }.font(KithText.secondary.font).foregroundStyle(Palette.accent)
                }
            }
            if let error = composer.error { Text(error).font(KithText.secondary.font).foregroundStyle(Palette.negative) }
            chips
            HStack(alignment: .bottom, spacing: 8) {
                TextField("Ask anyone", text: Binding(get: { composer.text }, set: { composer.edit($0) }), axis: .vertical)
                    .lineLimit(1...5)
                    .font(KithText.body.font)
                    .padding(.vertical, 10)
                Button { Task { await composer.send() } } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.system(size: 32))
                        .foregroundStyle(composer.canSend ? Palette.accentFill : Palette.faint)
                }
                .disabled(!composer.canSend)
                .accessibilityLabel("Send")
            }
            .padding(.leading, 16).padding(.trailing, 8)
            .kithGlassPill()
        }
        .padding(.horizontal, KithSpacing.gutter).padding(.bottom, 8)
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
