import KithKit
import SwiftUI

/// A topic the owner split off, folded into one card in the flat chat: the faces, the title, the newest line and a
/// count. Tapping opens the topic inline (`ThreadView.topicRow` lists the folded entries under it). Never a context
/// ring. A question inside that waits on the owner rises onto the card and is answered there.
struct TopicCard: View {
    let group: TopicGroup
    let chat: Bot?
    let expanded: Bool
    let toggle: () -> Void
    let rename: () -> Void
    let unfold: () -> Void
    @Environment(BotStore.self) private var model

    var body: some View {
        let summary = Topics.summary(of: group)
        let tint = AgentTint.resolve(summary.needsYou.first.flatMap { agent(of: $0)?.avatarColor })
        VStack(alignment: .leading, spacing: 10) {
            Button(action: toggle) { header(summary) }
                .buttonStyle(PressScale())
                .accessibilityLabel("\(group.topic.title), \(summary.count) messages\(summary.needsYou.isEmpty ? "" : ", needs you")")
                .accessibilityValue(expanded ? "Expanded" : "Collapsed")
                .accessibilityHint(expanded ? "Hides the topic" : "Shows the topic")
            ForEach(summary.needsYou) { entry in
                ChatRow(entry: entry, groupStart: false, chat: chat, openTrace: {})
            }
        }
        .padding(12)
        .background(summary.needsYou.isEmpty ? Palette.panel : tint.a.opacity(0.12),
                    in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
        .contextMenu {
            if model.isOwner {
                Button("Rename", systemImage: "pencil", action: rename)
                Button("Unfold", systemImage: "arrow.up.left.and.arrow.down.right", action: unfold)
            }
        }
        .padding(.top, 12)
        .padding(.bottom, 4)
    }

    private func header(_ summary: TopicSummary) -> some View {
        HStack(alignment: .center, spacing: 12) {
            faces(summary)
            VStack(alignment: .leading, spacing: 2) {
                Text(group.topic.title)
                    .font(.system(.subheadline, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(2)
                if !summary.latestLine.isEmpty {
                    Text(latest(summary))
                        .kithText(.secondary)
                        .foregroundStyle(Palette.secondary)
                        .lineLimit(1)
                }
                Text(summary.count == 1 ? "1 message" : "\(summary.count) messages")
                    .kithText(.secondary)
                    .foregroundStyle(Palette.muted)
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 6) {
                Text(summary.date.formatted(.dateTime.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits)))
                    .kithText(.code)
                    .foregroundStyle(Palette.muted)
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(Palette.secondary)
                    .rotationEffect(.degrees(expanded ? 90 : 0))
            }
        }
        .frame(minHeight: 44)
        .contentShape(Rectangle())
    }

    /// Up to two overlapping faces; dots under them while a folded turn is running.
    private func faces(_ summary: TopicSummary) -> some View {
        HStack(spacing: -10) {
            ForEach(Array(summary.authors.enumerated()), id: \.offset) { _, key in
                face(key)
            }
        }
        .overlay(alignment: .bottomTrailing) {
            if summary.working { WorkingDots(size: 36).offset(x: 4, y: 6) }
        }
    }

    @ViewBuilder private func face(_ key: String) -> some View {
        if key == "user" {
            Image(systemName: "person.fill")
                .font(.system(size: 14))
                .foregroundStyle(Palette.secondary)
                .frame(width: 32, height: 32)
                .background(Palette.bubbleUser, in: Circle())
                .overlay(Circle().strokeBorder(Palette.panel, lineWidth: 2))
        } else if let bot = agent(key) {
            CharacterAvatar(bot: bot, size: 32, animated: false)
                .overlay(Circle().strokeBorder(Palette.panel, lineWidth: 2))
        }
    }

    private func agent(_ key: String) -> Bot? {
        let id = String(key.dropFirst("agent:".count))
        return model.bots[id] ?? chat
    }

    private func agent(of entry: Entry) -> Bot? {
        entry.data.author.flatMap { model.bots[$0] } ?? chat
    }

    /// "designer: Safe enough at 0.85." The owner's own words carry "You".
    private func latest(_ summary: TopicSummary) -> String {
        guard let e = summary.latest else { return summary.latestLine }
        let name = e.kind == "user" ? "You" : (agent(of: e)?.name ?? "Agent")
        return "\(name): \(summary.latestLine)"
    }
}

/// Which message a topic title is being asked for, and whether it makes a topic or renames one.
struct TopicTitling: Identifiable {
    let entry: Entry
    let current: String
    var isNew = false
    var id: String { entry.id }
}

/// The title prompt: prefilled, editable, 80 characters at most. The host's refusal shows under the field.
struct TopicTitleSheet: View {
    let botId: String
    let request: TopicTitling
    @State private var title: String
    @State private var saving = false
    @State private var failure: String?
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss

    init(botId: String, request: TopicTitling) {
        self.botId = botId
        self.request = request
        _title = State(initialValue: request.current)
    }

    var body: some View {
        VStack(spacing: 0) {
            ModalHeader(request.isNew ? "Make a topic" : "Rename topic") {
                if saving {
                    Spinner()
                } else {
                    IconButton("Save", systemImage: "checkmark", action: save)
                        .disabled(trimmed.isEmpty)
                }
            }
            CardForm {
                CardSection(footer: failure ?? "This and the replies after it fold into one card in the chat.") {
                    TextField("Title", text: $title)
                        .onSubmit(save)
                        .submitLabel(.done)
                        .onChange(of: title) { _, new in
                            if new.count > Topics.titleLimit { title = String(new.prefix(Topics.titleLimit)) }
                        }
                }
            }
            .textFieldStyle(.plain)
        }
        .background(Palette.background)
    }

    private var trimmed: String { title.trimmingCharacters(in: .whitespacesAndNewlines) }

    private func save() {
        guard !trimmed.isEmpty, !saving else { return }
        saving = true
        failure = nil
        Task {
            let refused = request.isNew
                ? await model.makeTopic(request.entry, title: trimmed)
                : await model.renameTopic(request.entry, title: trimmed)
            if let refused { failure = refused; saving = false } else { dismiss() }
        }
    }
}
