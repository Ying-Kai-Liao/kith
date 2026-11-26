import KithKit
import SwiftUI

/// You > Routines: every routine of the computer's agents, one section per agent, each with an on/off switch.
/// A row opens the same editor as the agent's own Routines card. Loads each agent's list (the host answers per bot) and
/// reloads every few seconds, since a routine can also be switched from a chat card.
public struct RoutinesByAgentView: View {
    let store: BotStore
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var routines: [Routine] = []
    @State private var runs: [RoutineRun] = []
    @State private var loaded = false
    @State private var loadError: String?
    @State private var error: String?
    /// Routines whose switch is in flight: their switch is off until the host answers.
    @State private var toggling: Set<String> = []
    @State private var editing: Edit?

    public init(store: BotStore) { self.store = store }

    private struct Edit: Identifiable {
        let botId: String
        let routineId: String
        var id: String { routineId }
    }

    /// Agents (not rooms) by bot id.
    private var names: [String: String] {
        store.bots.values.filter { !$0.isGroup }.reduce(into: [:]) { $0[$1.id] = $1.name }
    }

    public var body: some View {
        let groups = RoutineGroup.group(routines, names: names)
        CardForm {
            if groups.isEmpty {
                CardSection {
                    Text(loaded ? "No routines yet. Ask an agent in its chat for one." : "Loading routines…")
                        .foregroundStyle(Palette.secondary)
                }
            }
            ForEach(groups) { group in
                CardSection(group.title) {
                    ForEach(group.routines) { routine in row(routine) }
                }
            }
            if let loadError { SheetNote(text: loadError, danger: true) { Task { await load() } } }
            if let error { SheetNote(text: error, danger: true) }
        }
        .foregroundStyle(Palette.text)
        .task(id: names.keys.sorted()) {
            repeat {
                await load()
                do { try await Task.sleep(for: .seconds(4)) } catch { break }
            } while !Task.isCancelled
        }
        .kithSheet(item: $editing) { target in
            RoutineEditorView(botId: target.botId, routine: routines.first { $0.id == target.routineId }) { _ in
                editing = nil
                Task { await load() }
            }
            .environment(store)
        }
    }

    private func row(_ routine: Routine) -> some View {
        HStack(spacing: 12) {
            Button { editing = Edit(botId: routine.botId, routineId: routine.id) } label: {
                SheetRow(icon: "clock", title: routine.name, detail: summary(routine))
            }
            .buttonStyle(.plain)
            Toggle(isOn: Binding(get: { routine.enabled }, set: { setEnabled(routine, $0) })) { EmptyView() }
                .toggleStyle(.kith)
                .fixedSize()
                .accessibilityLabel(routine.name)
                .disabled(toggling.contains(routine.id) || store.isOffline)
        }
    }

    /// What's happening now, else when it runs.
    private func summary(_ routine: Routine) -> String {
        if let active = runs.first(where: { $0.routineId == routine.id && $0.isActive }) { return Self.runLabel(active) }
        if routine.lastError != nil { return "Schedule needs attention" }
        if let last = runs.first(where: { $0.routineId == routine.id }), ["failed", "interrupted"].contains(last.status) {
            return "Last run \(Self.runLabel(last).lowercased())"
        }
        return routine.triggerDescriptions.joined(separator: " · ")
    }

    private static func runLabel(_ run: RoutineRun) -> String {
        switch run.status {
        case "pending": "Queued"
        case "starting": "Starting"
        case "running": "Running"
        case "recovering": "Resuming after restart"
        default: run.status.capitalized
        }
    }

    private func setEnabled(_ routine: Routine, _ enabled: Bool) {
        guard !toggling.contains(routine.id), let client = store.client else { return }
        toggling.insert(routine.id)
        Task {
            defer { toggling.remove(routine.id) }
            do {
                try await client.routineAction("setRoutineEnabled", botId: routine.botId, id: routine.id, enabled: enabled)
                error = nil
                await load()
            } catch { self.error = error.localizedDescription }
        }
    }

    private func load() async {
        guard let client = store.client else { return }
        let ids = names.keys.sorted()
        let results = await withTaskGroup(of: (RoutineListing?, Error?).self) { group in
            for id in ids {
                group.addTask {
                    do { return (try await client.routines(botId: id), nil) } catch { return (nil, error) }
                }
            }
            var out: [(RoutineListing?, Error?)] = []
            for await one in group { out.append(one) }
            return out
        }
        if Task.isCancelled { return }
        // One agent's failure doesn't hide the others' routines.
        routines = results.compactMap(\.0).flatMap(\.routines)
        runs = results.compactMap(\.0).flatMap(\.runs)
        loaded = true
        loadError = results.compactMap(\.1).first?.localizedDescription
    }
}
