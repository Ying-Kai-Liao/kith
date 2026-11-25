import KithKit
import KithUI
import SwiftUI

/// Share, in up to three steps: what (agents and rooms), with whom (someone on the computer or a new person), at which
/// level. Done shows the invite link for a new person. `prefill` starts it with those picked (skipping the first step);
/// `person` with someone chosen (skipping the second).
struct ShareFlowSheet: View {
    let service: any SharingService
    let store: BotStore
    var prefill: [ShareTarget] = []
    var person: String?

    private enum Step { case what, who, level, done }

    @Environment(\.dismissModal) private var dismiss
    @State private var step: Step
    @State private var picked: [ShareTarget]
    /// Nil is a new person.
    @State private var recipient: String?
    @State private var newName = ""
    @State private var level: ShareLevel = .chat
    @State private var outcome: ShareOutcome?
    @State private var working = false

    init(service: any SharingService, store: BotStore, prefill: [ShareTarget] = [], person: String? = nil) {
        self.service = service
        self.store = store
        self.prefill = prefill
        self.person = person
        _picked = State(initialValue: prefill)
        _recipient = State(initialValue: person)
        _step = State(initialValue: prefill.isEmpty ? .what : person == nil ? .who : .level)
    }

    /// The steps this share goes through: the first when nothing is chosen, the second when no person is, the last
    /// only while an agent is picked (rooms have no level).
    private var flow: [Step] {
        var steps: [Step] = []
        if prefill.isEmpty { steps.append(.what) }
        if person == nil { steps.append(.who) }
        if picked.contains(where: \.isAgent) { steps.append(.level) }
        return steps
    }
    private var isLast: Bool { flow.last == step || flow.isEmpty }
    private var hasAgent: Bool { picked.contains(where: \.isAgent) }
    private var typedName: String { newName.trimmingCharacters(in: .whitespaces) }

    private var canContinue: Bool {
        if working || service.busy { return false }
        switch step {
        case .what: return !picked.isEmpty
        case .who: return recipient != nil || (!typedName.isEmpty && hasAgent)
        case .level, .done: return true
        }
    }

    private var title: String {
        switch step {
        case .what: "Share what?"
        case .who: "Share with whom?"
        case .level: "How much can they do?"
        case .done: "Shared"
        }
    }

    private var recipientName: String {
        recipient.flatMap { service.person($0)?.name } ?? typedName
    }

    var body: some View {
        Group {
            switch step {
            case .what: whatStep
            case .who: whoStep
            case .level: levelStep
            case .done: doneStep
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader(title) }
        .task { await service.reload() }
    }

    // MARK: steps

    private var whatStep: some View {
        CardForm {
            CardSection("Agents", footer: service.agents.isEmpty ? "No agents on this computer yet." : nil) {
                ForEach(service.agents, id: \.self) { toggle(.agent($0)) }
            }
            if !service.rooms.isEmpty {
                CardSection("Rooms") {
                    ForEach(service.rooms, id: \.self) { toggle(.room($0)) }
                }
            }
            navigation
        }
    }

    private func toggle(_ what: ShareTarget) -> some View {
        Toggle(what.name, isOn: Binding(get: { picked.contains(what) }, set: { on in
            picked.removeAll { $0 == what }
            if on { picked.append(what) }
        }))
        .toggleStyle(.kith)
    }

    private var whoStep: some View {
        CardForm {
            CardSection("Who", footer: "They sign in to Kith with their own Google, Apple or GitHub account.") {
                ChoicePicker(selection: Binding(get: { recipient ?? "" }, set: { recipient = $0.isEmpty ? nil : $0 }),
                             options: [(id: "", label: "New person")] + service.persons.filter { $0.status != .removed }.map { (id: $0.id, label: $0.name) })
                if recipient == nil {
                    TextField("Name", text: $newName)
                        .textInputAutocapitalization(.words)
                        .submitLabel(.done)
                }
            }
            if recipient == nil, !hasAgent {
                Text("A new person needs an agent. Go back and pick one, or choose someone who is already here.")
                    .font(.footnote).foregroundStyle(Palette.secondary)
            }
            navigation
        }
    }

    private var levelStep: some View {
        CardForm {
            CardSection("Level", footer: picked.contains { !$0.isAgent } ? "Rooms are shared, with no level." : nil) {
                SegmentedChoice(selection: $level, options: ShareLevel.allCases.map { ($0, $0.title) })
                ForEach(ShareLevel.allCases, id: \.self) { option in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(option.title).font(.subheadline.weight(.semibold))
                        Text(option.detail).font(.subheadline).foregroundStyle(Palette.secondary)
                    }
                    .opacity(option == level ? 1 : 0.55)
                }
            }
            navigation
        }
    }

    private var doneStep: some View {
        Group {
            if case .invite(let made)? = outcome {
                InviteLinkView(invite: made, name: recipientName)
            } else {
                CardForm {
                    CardSection {
                        Label("Shared with \(recipientName).", systemImage: "checkmark.circle")
                            .foregroundStyle(Palette.text)
                    }
                    Button { dismiss() } label: { Text("Done").frame(maxWidth: .infinity, minHeight: 28) }
                        .buttonStyle(.primary)
                }
            }
        }
    }

    // MARK: moving on

    private var navigation: some View {
        VStack(spacing: 12) {
            if let error = service.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            Button { Task { await forward() } } label: {
                ZStack {
                    Text(isLast ? "Share" : "Next").opacity(working ? 0 : 1)
                    if working { Spinner(size: 18) }
                }
                .frame(maxWidth: .infinity, minHeight: 28)
            }
            .buttonStyle(.primary)
            .disabled(!canContinue)
            if let back = previous {
                Button("Back") { step = back }.buttonStyle(.plain).foregroundStyle(Palette.secondary)
            }
        }
    }

    private var previous: Step? {
        guard let i = flow.firstIndex(of: step), i > 0 else { return nil }
        return flow[i - 1]
    }

    private func forward() async {
        guard !working else { return }
        if !isLast, let i = flow.firstIndex(of: step) {
            step = flow[i + 1]
            return
        }
        working = true
        defer { working = false }
        let to: ShareRecipient = recipient.map { .existing($0) } ?? .new(name: typedName)
        if let made = await service.share(picked, with: to, level: level) {
            outcome = made
            step = .done
        }
    }
}
