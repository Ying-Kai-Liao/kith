import KithKit
import SwiftUI

// The report cards drawn over an entry's `data.richCard` (DESIGN.md "Schedule" and "News"): panel grey, never tinted,
// waiting on nobody. The words come from `ScheduleCardModel` and `NewsCardModel`; buttons are a later step.

extension RichCard {
    /// A Schedule or News card this app draws in place of the entry's text twin.
    var drawsInChat: Bool { (type == "schedule" || type == "news" || type == "plan") && isDrawable() }
}

/// A card folded to one line: a changed or removed job, or a card after its day (tap opens it in place).
private struct FoldedRichCard: View {
    let fold: ScheduleCardModel.Fold
    let open: () -> Void

    var body: some View {
        if fold.opens {
            Button(action: open) { pill }.buttonStyle(.plain)
        } else {
            pill
        }
    }

    private var pill: some View {
        let parts = fold.text.split(separator: " · ", maxSplits: 1).map(String.init)
        return HStack(spacing: 6) {
            if fold.done {
                Image(systemName: "checkmark").font(.system(size: 10, weight: .bold)).foregroundStyle(Palette.positive)
            }
            Image(systemName: "clock").font(.system(size: 11)).foregroundStyle(Palette.muted)
            HStack(spacing: 0) {
                Text(parts.first ?? "").fontWeight(.semibold).foregroundStyle(Palette.ink)
                if parts.count > 1 { Text(" · \(parts[1])").foregroundStyle(Palette.muted) }
            }
            .kithText(.secondary)
            .lineLimit(1)
            Spacer(minLength: 8)
            Text(fold.trailing).kithText(.secondary).monospacedDigit().foregroundStyle(Palette.muted)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Palette.panel, in: Capsule())
        .contentShape(Capsule())
        .accessibilityElement(children: .combine)
    }
}

/// The card's grey panel and its clock header.
private struct ReportCard<Trailing: View, Content: View>: View {
    let title: String
    var icon = "clock"
    @ViewBuilder var trailing: Trailing
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: icon).font(.system(size: 12)).foregroundStyle(Palette.ink)
                Text(title).kithText(.secondary).fontWeight(.semibold).foregroundStyle(Palette.ink).lineLimit(1)
                Spacer(minLength: 8)
                trailing
            }
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 12)
        .padding(.vertical, 11)
        .background(Palette.panel, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityElement(children: .contain)
    }
}

/// The white group inside a card, rows split by hairlines.
private struct WhiteGroup<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Group(subviews: content) { rows in
                ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                    if index > 0 { Hairline() }
                    row
                }
            }
        }
        .background(Palette.background, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

// MARK: Schedule

/// The Schedule card: a Kith agent's kith-sched job or an outside bot's routine. The owner sees the job live (read once,
/// then about every 30 s while the card is open); anyone else sees the entry's snapshot.
struct ScheduleCard: View {
    let entry: Entry
    let card: RichCard
    @State private var opened = false
    @State private var allTask = false
    /// `.none` until read (the snapshot shows); `.some(nil)` once read and gone.
    @State private var live: ScheduleLive?? = .none
    @State private var kith: KithInfoStore?
    @Environment(BotStore.self) private var model

    private static let pollEvery: Duration = .seconds(30)

    private var ref: [String: RichCard.JSON] { card.ref ?? [:] }
    private func refString(_ key: String) -> String? {
        if case let .string(s)? = ref[key], !s.isEmpty { return s }
        return nil
    }

    private var person: String? {
        if case let .string(s)? = card.snapshot?["person"], !s.isEmpty { return s }
        return nil
    }

    var body: some View {
        // The card posts in the 1:1 of the person the job runs as: theirs when it names them, else the owner's.
        let view = ScheduleCardModel(card: card, postedAt: entry.date, live: live,
                                     viewerIsPerson: person != nil ? !model.isOwner : model.isOwner,
                                     personName: person.map { $0.prefix(1).uppercased() + $0.dropFirst() }, opened: opened)
        Group {
            if let fold = view.fold {
                FoldedRichCard(fold: fold) { opened = true }
            } else {
                full(view)
            }
        }
        .task(id: view.fold == nil) { await follow(polling: view.fold == nil) }
    }

    private func full(_ view: ScheduleCardModel) -> some View {
        ReportCard(title: view.name) {
            if view.paused {
                Text("Paused").kithText(.label).foregroundStyle(Palette.muted)
                    .padding(.horizontal, 7).padding(.vertical, 2)
                    .background(Palette.line, in: Capsule())
            } else if let op = view.op {
                Text(op).font(.caption).monospacedDigit().foregroundStyle(Palette.muted).lineLimit(1)
            }
        } content: {
            Text(view.every).kithText(.body).fontWeight(.medium)
                .foregroundStyle(view.paused ? Palette.muted : Palette.ink)
            WhiteGroup {
                row("Next") { Text(view.next).monospacedDigit() }
                row("Last") {
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 6) {
                            if view.last.running { WorkingDots(size: 30) }
                            Text(view.last.text).monospacedDigit()
                            if view.last.failed { Text("· Failed").foregroundStyle(Palette.negative) }
                        }
                        if let error = view.last.error {
                            Text(error).font(.caption).foregroundStyle(Palette.muted).lineLimit(1)
                        }
                    }
                }
                row("Runs") { runs(view) }
            }
            if !view.task.isEmpty {
                Button { allTask.toggle() } label: {
                    Text(view.task).font(.caption).foregroundStyle(Palette.muted)
                        .lineLimit(allTask ? nil : 2)
                        .multilineTextAlignment(.leading)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .buttonStyle(.plain)
            }
            ScheduleButtons(view: view, ownerName: ScheduleFoot.ownerName(from: model.bots[entry.botId]?.activity),
                            act: model.isOwner && canAct ? { try await act($0) } : nil)
        }
        .accessibilityLabel("Schedule: \(view.name)")
    }

    private func row<Value: View>(_ key: String, @ViewBuilder value: () -> Value) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(key).foregroundStyle(Palette.muted).frame(width: 36, alignment: .leading)
            value().foregroundStyle(Palette.ink)
            Spacer(minLength: 0)
        }
        .kithText(.secondary)
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
    }

    private func runs(_ view: ScheduleCardModel) -> some View {
        let runner = view.kind == .routine ? refString("bot").flatMap { model.bots[$0] } : jobBot
        return HStack(spacing: 5) {
            if let runner { CharacterAvatar(bot: runner, size: 16, animated: false) }
            Text(runner?.name ?? refString("agent") ?? "The bot").lineLimit(1)
            if let runsAs = view.runsAs {
                Text("as").foregroundStyle(Palette.muted)
                Text(runsAs).lineLimit(1)
            }
        }
    }

    /// The bot a Kith job's agent is, else the chat the card is in.
    private var jobBot: Bot? {
        let agent = refString("agent")
        return model.bots.values.first { agent != nil && $0.hqAgent == agent } ?? model.bots[entry.botId]
    }

    /// The owner reads the job live: once, then every 30 s while the card is open. Members keep the snapshot.
    private func follow(polling: Bool) async {
        guard model.isOwner else { return }
        repeat {
            await read()
            guard polling else { return }
            try? await Task.sleep(for: Self.pollEvery)
        } while !Task.isCancelled
    }

    private func read() async {
        if let routine = refString("routine") {
            guard let bot = refString("bot"), let client = model.client,
                  let listing = try? await client.routines(botId: bot), !Task.isCancelled else { return }
            live = .some(ScheduleLive.routine(listing, id: routine))
            return
        }
        guard let id = refString("id"), let bot = jobBot else { return }
        let store = kithStore(bot)
        await store.load()
        if let info = store.info { live = .some(ScheduleLive.kith(info.schedules, id: id)) }
    }

    private func kithStore(_ bot: Bot) -> KithInfoStore {
        let store = kith ?? KithInfoStore(botId: bot.id) { try await model.ready() }
        kith = store
        return store
    }

    private var canAct: Bool {
        refString("routine") != nil ? refString("bot") != nil : refString("id") != nil && jobBot != nil
    }

    /// The owner's buttons: the routines API for a routine, kith-sched for a Kith job. The card then shows the job
    /// read back, also after a failed call.
    private func act(_ op: ScheduleOp) async throws {
        if let routine = refString("routine") {
            guard let bot = refString("bot"), let client = model.client else { return }
            let method = switch op {
            case .pause, .resume: "setRoutineEnabled"
            case .run: "runRoutine"
            case .remove: "deleteRoutine"
            }
            do {
                try await client.routineAction(method, botId: bot, id: routine,
                                               enabled: op == .pause || op == .resume ? op == .resume : nil)
            } catch {
                await read()
                throw error
            }
            await read()
            return
        }
        guard let id = refString("id"), let bot = jobBot else { return }
        let store = kithStore(bot)
        try await store.schedule(id, op: op)
        if let info = store.info { live = .some(ScheduleLive.kith(info.schedules, id: id)) }
    }
}

/// The card's foot: Pause / Run now and the trash for the owner (Remove asks inside the card), a line for anyone else.
private struct ScheduleButtons: View {
    let view: ScheduleCardModel
    let ownerName: String?
    /// The owner's buttons; nil for anyone else. Throws why a call failed; the card then shows the job read back.
    let act: ((ScheduleOp) async throws -> Void)?
    @State private var confirming = false
    @State private var busy: ScheduleOp?
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            switch ScheduleFoot(view, owner: act != nil, ownerName: ownerName, confirming: confirming, busy: busy) {
            case let .note(text):
                Text(text).font(.caption).foregroundStyle(Palette.muted)
            case let .confirm(question, removing):
                Text(question).kithText(.secondary).foregroundStyle(Palette.ink)
                HStack(spacing: 8) {
                    Button { press(.remove) } label: { label("Remove") }.buttonStyle(.danger)
                    Button { confirming = false; error = nil } label: { label("Cancel") }.buttonStyle(.line)
                }
                .disabled(removing)
            case let .buttons(toggle, run, runDisabled, busy):
                HStack(spacing: 8) {
                    if toggle.ink {
                        Button { press(toggle.op) } label: { label(toggle.label) }.buttonStyle(.ink).disabled(busy)
                    } else {
                        Button { press(toggle.op) } label: { label(toggle.label) }.buttonStyle(.line).disabled(busy)
                    }
                    Button { press(.run) } label: { label(run) }.buttonStyle(.line).disabled(runDisabled)
                    Spacer(minLength: 0)
                    Button { confirming = true; error = nil } label: {
                        Image(systemName: "trash").font(.footnote).foregroundStyle(Palette.muted).frame(width: 32, height: 32)
                    }
                    .buttonStyle(.plain)
                    .disabled(busy)
                    .accessibilityLabel("Remove \(view.name)")
                }
            }
            if let error {
                Text(error).font(.caption).foregroundStyle(Palette.negative)
            }
        }
    }

    private func label(_ text: String) -> some View {
        Text(text).kithText(.secondary).fontWeight(.medium).padding(.horizontal, 14).frame(minHeight: 32)
    }

    private func press(_ op: ScheduleOp) {
        guard let act, busy == nil else { return }
        busy = op
        error = nil
        Task {
            do {
                try await act(op)
                confirming = false
            } catch {
                self.error = error.localizedDescription
            }
            busy = nil
        }
    }
}

// MARK: News

/// Links opened this session go quiet, so a second look shows what's left.
@MainActor private enum OpenedLinks {
    static var urls: Set<URL> = []
}

private struct NewsItem: View {
    let row: NewsCardModel.Row
    @State private var quiet = false
    @Environment(\.openURL) private var openURL

    var body: some View {
        Button {
            OpenedLinks.urls.insert(row.url)
            quiet = true
            openURL(row.url)
        } label: {
            HStack(alignment: .top, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(row.title).kithText(.secondary).fontWeight(.semibold).lineLimit(2)
                    if !row.line.isEmpty { Text(row.line).kithText(.secondary).lineLimit(1) }
                    HStack(spacing: 0) {
                        if let source = row.source { Text("\(source) · ") }
                        Text(row.domain).font(KithText.code.font)
                    }
                    .font(.caption)
                    .foregroundStyle(Palette.muted)
                    .lineLimit(1)
                }
                .foregroundStyle(quiet ? Palette.muted : Palette.ink)
                Spacer(minLength: 0)
                Image(systemName: "arrow.up.right").font(.system(size: 9)).foregroundStyle(Palette.muted)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint(row.url.host() ?? row.domain)
        .onAppear { quiet = OpenedLinks.urls.contains(row.url) }
    }
}

/// The News card: a scheduled run's brief as rows that open their links, its lead and tail as the agent's bubbles.
struct NewsCard: View {
    let entry: Entry
    let card: RichCard
    @State private var expanded = false
    @State private var opened = false

    var body: some View {
        let view = NewsCardModel(card: card, postedAt: entry.date, expanded: expanded, opened: opened)
        if let fold = view.fold {
            FoldedRichCard(fold: .init(text: fold, trailing: CardWords.clock(entry.date), opens: true, done: false)) { opened = true }
        } else {
            VStack(alignment: .leading, spacing: 6) {
                if !view.lead.isEmpty { CardBubble(text: view.lead) }
                ReportCard(title: view.title) {
                    if let every = view.every {
                        Text(every).font(.caption).monospacedDigit().foregroundStyle(Palette.muted).lineLimit(1)
                    }
                } content: {
                    WhiteGroup {
                        ForEach(Array(view.shown.enumerated()), id: \.offset) { _, row in NewsItem(row: row) }
                        if view.more > 0 {
                            Button { expanded = true } label: {
                                HStack(spacing: 4) {
                                    Text("Show \(view.more) more")
                                    Image(systemName: "chevron.down").font(.system(size: 9, weight: .semibold))
                                }
                                .kithText(.secondary)
                                .foregroundStyle(Palette.muted)
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 8)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
                .accessibilityLabel(view.title)
                if !view.tail.isEmpty { CardBubble(text: view.tail) }
            }
        }
    }
}

/// The brief's own words around the card, in the agent's bubble.
private struct CardBubble: View {
    let text: String

    var body: some View {
        MarkdownText(text).equatable()
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: KithRadius.card + 4, style: .continuous))
            .padding(.trailing, 40)
            .textSelection(.enabled)
    }
}

// MARK: Plan

/// The Plan card: a count in mono, a 4 pt bar in the agent's pair, and steps that change in place (done = check, muted;
/// now = spinner, semibold; next = ring, light). A turn that ended mid-plan leaves its step as a dashed ring.
struct PlanCard: View {
    let entry: Entry
    let card: RichCard
    @Environment(BotStore.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let view = PlanCardModel(card: card)
        let tint = AgentTint.resolve((entry.data.author.flatMap { model.bots[$0] } ?? model.bots[entry.botId])?.avatarColor)
        ReportCard(title: view.title, icon: "checklist") {
            Text(view.count).font(.caption.monospaced()).foregroundStyle(Palette.muted).lineLimit(1)
        } content: {
            GeometryReader { geo in
                Capsule().fill(Palette.line)
                    .overlay(alignment: .leading) {
                        Capsule().fill(LinearGradient(colors: [tint.a, tint.b], startPoint: .leading, endPoint: .trailing))
                            .frame(width: geo.size.width * view.fraction)
                    }
            }
            .frame(height: 4)
            .animation(Motion.reduced(Motion.layout, reduceMotion), value: view.fraction)
            VStack(alignment: .leading, spacing: 6) {
                ForEach(view.steps) { step in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        mark(step.state, ended: view.ended).frame(width: 14)
                        Text(step.text).kithText(.secondary)
                            .fontWeight(step.state == .now ? .semibold : step.state == .next ? .light : .regular)
                            .foregroundStyle(step.state == .done || step.state == .stopped ? Palette.muted : Palette.ink)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityValue(step.state.rawValue)
                }
            }
        }
        .accessibilityLabel("\(view.title), \(view.count)")
    }

    @ViewBuilder private func mark(_ state: PlanCardModel.State, ended: Bool) -> some View {
        switch state {
        case .done:
            Image(systemName: "checkmark").font(.system(size: 10, weight: .bold)).foregroundStyle(Palette.muted)
        case .now where !ended:
            ProgressView().controlSize(.mini)
        case .stopped, .now:
            Circle().strokeBorder(Palette.muted, style: StrokeStyle(lineWidth: 1.5, dash: [2, 2])).frame(width: 11, height: 11)
        case .next:
            Circle().strokeBorder(Palette.line, lineWidth: 1.5).frame(width: 11, height: 11)
        }
    }
}
