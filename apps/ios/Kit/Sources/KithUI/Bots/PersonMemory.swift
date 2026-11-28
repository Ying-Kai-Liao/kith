import KithKit
import Observation
import SwiftUI

/// What one agent keeps about one person (`people/<person>/` in its memory), and forgetting it. Owner only.
/// Forget runs once at a time (`busy`), and the files it removed can be put back with `undo` until the next forget.
@MainActor @Observable
public final class PersonMemoryModel {
    public struct Undo: Equatable {
        public let id = UUID()
        public var removed: [HqPersonForget.Removed]
        /// "Sam's notes" or "3 files": what the banner says was forgotten.
        public var label: String
    }

    let personId: String
    private let list: () async throws -> HqPersonMemory
    private let forgetCall: (String?) async throws -> HqPersonForget
    private let restoreCall: (String, Int64) async throws -> Void

    public private(set) var files: [HqMemoryFileEntry] = []
    /// False when the host ignores the person filter (before 0.4): there is nothing of the person's to show.
    public private(set) var supported = true
    public private(set) var loaded = false
    public private(set) var busy = false
    public private(set) var error: String?
    public var undo: Undo?

    public init(personId: String,
                list: @escaping () async throws -> HqPersonMemory,
                forget: @escaping (String?) async throws -> HqPersonForget,
                restore: @escaping (String, Int64) async throws -> Void) {
        self.personId = personId
        self.list = list
        self.forgetCall = forget
        self.restoreCall = restore
    }

    /// Whether the row is worth showing: a host that can filter, once it has answered.
    public var visible: Bool { loaded && supported }

    public func load() async {
        do {
            let answer = try await list()
            supported = answer.supported
            files = answer.files(of: personId)
            error = nil
        } catch {
            if error is HqMemoryUnsupported { supported = false } else { self.error = error.localizedDescription }
        }
        loaded = true
    }

    /// One file, by its full name (`people/sam/notes.md`).
    public func forget(file: HqMemoryFileEntry) async {
        let prefix = HqPersonMemory.prefix(personId)
        await forget(name: file.name.hasPrefix(prefix) ? String(file.name.dropFirst(prefix.count)) : file.name,
                     label: PersonMemorySheet.noteTitle(file.name))
    }

    public func forgetAll(name: String) async { await forget(name: nil, label: "everything about \(name)") }

    private func forget(name: String?, label: String) async {
        guard !busy else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            let done = try await forgetCall(name)
            undo = done.removed.isEmpty ? nil : Undo(removed: done.removed, label: label)
            await load()
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Puts back what the last forget removed. A file that can't come back stays in the banner to try again.
    public func restore() async {
        guard !busy, let pending = undo else { return }
        busy = true
        error = nil
        defer { busy = false }
        var left: [HqPersonForget.Removed] = []
        for item in pending.removed {
            guard let version = item.version else { continue }
            do { try await restoreCall(item.name, version) } catch {
                left.append(item)
                self.error = error.localizedDescription
            }
        }
        undo = left.isEmpty ? nil : Undo(removed: left, label: pending.label)
        await load()
    }
}

/// "Memory" on the person page: for each agent they hold, the notes it keeps about them.
/// "Memory" on the person page: one row per agent they hold ("3 notes"), each opening that agent's notes about them.
public struct PersonMemorySection: View {
    let personName: String
    let personId: String
    let agents: [String]
    let store: BotStore
    /// Debug demos: made-up notes per agent, no host.
    let demo: [String: [String]]?
    @State private var models: [String: PersonMemoryModel] = [:]
    @State private var memories: [String: HqMemoryStore] = [:]
    @State private var open: String?

    public init(personName: String, personId: String, agents: [String], store: BotStore, demo: [String: [String]]? = nil) {
        self.personName = personName
        self.personId = personId
        self.agents = agents
        self.store = store
        self.demo = demo
    }

    private func bot(_ agent: String) -> Bot? { store.bots.values.first { $0.hqAgent == agent } }
    private func shown(_ agent: String) -> PersonMemoryModel? { models[agent].flatMap { $0.visible ? $0 : nil } }
    private func title(_ agent: String) -> String { bot(agent)?.name ?? agent }

    /// Reloads when any of these agents' memory changes.
    private var revs: String { agents.map { "\($0)\(bot($0)?.memoryRev ?? 0)" }.joined(separator: ",") }

    public var body: some View {
        // A container that always exists, so the load below runs while the section is still hidden.
        ZStack {
            if store.isOwner, agents.contains(where: { shown($0) != nil }) {
                CardSection("Memory", footer: "What each agent has written about \(personName). Tap one to read or forget it.") {
                    ForEach(agents.filter { shown($0) != nil }, id: \.self) { agent in
                        if let model = shown(agent) {
                            Button { open = agent } label: {
                                SheetRow(title: title(agent),
                                         detail: model.files.isEmpty ? "Nothing yet" : countLine(model.files.count),
                                         chevron: !model.files.isEmpty)
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .disabled(model.files.isEmpty)
                        }
                    }
                }
            }
        }
        .task(id: revs) {
            guard store.isOwner else { return }
            for agent in agents {
                let model = models[agent] ?? makeModel(agent)
                models[agent] = model
                await model.load()
            }
        }
        .kithSheet(item: Binding(get: { open.map(Opened.init) }, set: { open = $0?.agent })) { opened in
            if let model = models[opened.agent] {
                PersonMemorySheet(title: title(opened.agent), personName: personName, model: model,
                                  memory: memories[opened.agent], offline: store.isOffline, hostName: store.hostName)
            }
        }
    }

    private struct Opened: Identifiable { let agent: String; var id: String { agent } }

    private func countLine(_ n: Int) -> String { n == 1 ? "1 note" : "\(n) notes" }

    private func makeModel(_ agent: String) -> PersonMemoryModel {
        #if DEBUG
        if let notes = demo?[agent] { return PersonMemoryModel.demo(personId: personId, names: notes) }
        #endif
        guard let botId = bot(agent)?.id else {
            // No bot for this agent here: nothing to ask, nothing to show.
            return PersonMemoryModel(personId: personId, list: { throw HqMemoryUnsupported() },
                                     forget: { _ in throw HqMemoryUnsupported() }, restore: { _, _ in })
        }
        memories[agent] = HqMemoryStore(botId: botId) { try await store.ready() }
        let person = personId
        return PersonMemoryModel(
            personId: person,
            list: { try await store.ready().hqPersonMemory(botId: botId, person: person) },
            forget: { try await store.ready().hqPersonForget(botId: botId, person: person, name: $0) },
            restore: { name, version in _ = try await store.ready().hqMemoryRestore(botId: botId, shared: false, name: name, version: version) })
    }
}

/// One agent's notes about the person: forget each, or all, with an Undo for a while.
struct PersonMemorySheet: View {
    let title: String
    let personName: String
    let model: PersonMemoryModel
    let memory: HqMemoryStore?
    let offline: Bool
    let hostName: String
    @Environment(\.dismissModal) private var dismiss
    @State private var target: HqMemoryTarget?
    @State private var confirmAll = false

    private var disabled: Bool { model.busy || offline }

    var body: some View {
        VStack(spacing: 0) {
            ModalHeader("\(title) remembers")
            CardForm {
                if let undo = model.undo { undoRow(undo) }
                if let error = model.error { CardSection { SheetNote(text: error, danger: true) } }
                CardSection(footer: offline ? "\(hostName) is offline. You can change this when it is back."
                                : "Only what \(title) wrote about \(personName).") {
                    if model.files.isEmpty {
                        Text("Nothing yet").font(.subheadline).foregroundStyle(Palette.secondary)
                    }
                    ForEach(model.files) { file in fileRow(file) }
                }
                if !model.files.isEmpty {
                    CardSection {
                        Button { confirmAll = true } label: {
                            Label("Forget everything about \(personName)", systemImage: "eraser")
                                .foregroundStyle(Palette.danger)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(disabled)
                    }
                }
            }
        }
        .background(Palette.background)
        .kithSheet(item: $target) { target in
            if let memory { HqMemoryFileView(store: memory, target: target) }
        }
        .kithDialog("Forget everything about \(personName)?", isPresented: $confirmAll,
                    message: "\(title) loses these notes. You can undo this for a while.") {
            [DialogAction("Forget", destructive: true) { Task { await model.forgetAll(name: personName) } }]
        }
        .onChange(of: target) { _, new in
            if new == nil { Task { await model.load() } }
        }
    }

    private func fileRow(_ file: HqMemoryFileEntry) -> some View {
        HStack(spacing: 8) {
            Button { target = HqMemoryTarget(shared: false, name: file.name) } label: {
                SheetRow(title: noteTitle(file.name), detail: caption(file), chevron: true)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(memory == nil)
            Button("Forget") { Task { await model.forget(file: file) } }
                .buttonStyle(.plain)
                .font(.system(size: 12.5, weight: .semibold))
                .foregroundStyle(disabled ? Palette.secondary : Palette.danger)
                .disabled(disabled)
        }
    }

    private func undoRow(_ undo: PersonMemoryModel.Undo) -> some View {
        CardSection {
            HStack {
                Text("Forgot \(undo.label)").font(.subheadline).foregroundStyle(Palette.text)
                Spacer()
                Button("Undo") { Task { await model.restore() } }
                    .buttonStyle(.plain)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Palette.accent)
                    .disabled(model.busy)
            }
        }
        // The banner stays a while, not for good.
        .task(id: undo.id) {
            try? await Task.sleep(for: .seconds(30))
            if model.undo?.id == undo.id { model.undo = nil }
        }
    }

    /// "trip-plans.md" reads as "Trip plans".
    static func noteTitle(_ name: String) -> String {
        var base = name.split(separator: "/").last.map(String.init) ?? name
        if base.lowercased().hasSuffix(".md") { base.removeLast(3) }
        let words = base.replacingOccurrences(of: "-", with: " ").replacingOccurrences(of: "_", with: " ")
        return words.prefix(1).uppercased() + words.dropFirst()
    }

    private func noteTitle(_ name: String) -> String { Self.noteTitle(name) }

    private func caption(_ file: HqMemoryFileEntry) -> String? {
        guard file.modified > 0 else { return nil }
        return "Updated \(Date(milliseconds: file.modified).formatted(.relative(presentation: .named)))"
    }
}

#if DEBUG
extension PersonMemoryModel {
    /// The demo shots: made-up files that forget and come back in memory, no host.
    static func demo(personId: String, names: [String]) -> PersonMemoryModel {
        let box = DemoFiles(personId: personId, names: names)
        return PersonMemoryModel(
            personId: personId,
            list: { box.listed },
            forget: { box.forget($0) },
            restore: { name, _ in box.back(name) })
    }

    private final class DemoFiles: @unchecked Sendable {
        let personId: String
        var all: [HqMemoryFileEntry]
        var gone: [HqMemoryFileEntry] = []

        init(personId: String, names: [String]) {
            self.personId = personId
            let now = Int64(Date().timeIntervalSince1970 * 1000)
            all = names.enumerated().map {
                HqMemoryFileEntry(name: HqPersonMemory.prefix(personId) + $1, shared: false, size: 400 + $0 * 310,
                                  modified: now - Int64($0 + 1) * 3_600_000 * 20)
            }
        }

        var listed: HqPersonMemory { HqPersonMemory(files: all) }

        func forget(_ name: String?) -> HqPersonForget {
            let prefix = HqPersonMemory.prefix(personId)
            let hit = all.filter { name == nil || $0.name == prefix + name! }
            gone += hit
            all.removeAll { f in hit.contains { $0.name == f.name } }
            return HqPersonForget(person: personId, removed: hit.enumerated().map { .init(name: $1.name, version: Int64($0 + 1)) })
        }

        func back(_ name: String) {
            guard let file = gone.first(where: { $0.name == name }) else { return }
            gone.removeAll { $0.name == name }
            all.append(file)
        }
    }
}
#endif
