import KithKit
import SwiftUI

/// What the file sheet opens: one memory file, near a line when it came from a search hit.
struct HqMemoryTarget: Identifiable, Hashable {
    let shared: Bool
    let name: String
    var line: Int?
    var id: String { "\(shared ? "shared" : "agent"):\(name):\(line ?? 0)" }
}

/// An hq agent's Memory page: one memory, the agent's plain files. "About you" (shared with the user's other agents),
/// "This agent", and the "Tidy-ups" the host makes in the background.
struct HqMemoryCard: View {
    let botId: String
    let kith: KithInfoStore?
    @Environment(BotStore.self) private var host
    @State private var store: HqMemoryStore?
    @State private var target: HqMemoryTarget?
    @State private var undoing: HqTidyUp?
    @State private var fact = ""
    @State private var forgotten: String?

    var body: some View {
        // One page, as the mock draws it: search, Files, About you, then History.
        ScrollViewReader { proxy in
            VStack(alignment: .leading, spacing: 24) {
                if let store {
                    if let error = store.writeError {
                        SheetNote(text: error, danger: true)
                    }
                    if let kith {
                        KithMemorySection(store: kith) { path, line in
                            // Only Markdown memory files open in the editor.
                            guard path.lowercased().hasSuffix(".md") else { return }
                            withAnimation(Motion.layout) { target = HqMemoryTarget(shared: false, name: path, line: line) }
                        }
                    }
                    CardSection("Files", footer: "What this agent keeps in its own memory folder, starting with MEMORY.md.") {
                        fileRows(store, shared: false)
                    } accessory: {
                        Button("History") { withAnimation(Motion.layout) { proxy.scrollTo("memory-history", anchor: .top) } }
                            .buttonStyle(.plain)
                            .font(.system(size: 12, weight: .medium))
                            .foregroundStyle(Palette.accent)
                    }
                    CardSection("Forget a fact", footer: forgotten.map { "No agent will write “\($0)” to memory again. Lines already written stay until you edit them." }
                                    ?? "Keeps a fact out of every agent's memory from now on.") {
                        forgetRow(store)
                    }
                    CardSection("About you", footer: "What you've told your agents about you: your profile, people and projects. Every agent reads it.") {
                        fileRows(store, shared: true)
                    }
                    CardSection("History", footer: "Once you've chatted and the agent has been idle for a few minutes, it tidies its notes. Each tidy can be undone.") {
                        tidyRows(store)
                    }
                    .id("memory-history")
                } else {
                    CardSection { SheetNote(text: "Loading memory…") }
                }
            }
        }
        // Memory changes anywhere (the agent, a tidy-up, another device) move the bot's `memoryRev`: read again.
        .task(id: "\(botId)-\(host.bots[botId]?.memoryRev ?? 0)") {
            let current = store ?? HqMemoryStore(botId: botId) { try await host.ready() }
            store = current
            await current.load()
        }
        .kithSheet(item: $target) { target in
            if let store { HqMemoryFileView(store: store, target: target) }
        }
        .kithDialog("Undo this tidy?", isPresented: Binding(get: { undoing != nil }, set: { if !$0 { undoing = nil } }),
                      message: "Puts back every file it changed. A file that changed since is left alone.") {
            [DialogAction("Undo this tidy", destructive: true) {
                if let run = undoing { Task { await store?.undoTidy(run) } }
            }]
        }
    }

    // MARK: forget

    @ViewBuilder private func forgetRow(_ store: HqMemoryStore) -> some View {
        HStack(spacing: 8) {
            TextField("e.g. lives in Osaka", text: $fact)
                .textFieldStyle(.plain)
                .submitLabel(.done)
                .disabled(store.busy)
            Button("Forget") {
                let text = fact
                Task { if await store.forget(text) { forgotten = text.trimmingCharacters(in: .whitespacesAndNewlines); fact = "" } }
            }
            .buttonStyle(.plain)
            .font(.system(size: 12.5, weight: .semibold))
            .foregroundStyle(Palette.accent)
            .disabled(store.busy || fact.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    // MARK: files

    @ViewBuilder private func fileRows(_ store: HqMemoryStore, shared: Bool) -> some View {
        if let error = store.listError {
            SheetNote(text: error, danger: true)
        }
        if let list = store.list {
            let entries = shared ? list.shared : list.agent
            if entries.isEmpty {
                SheetNote(text: shared ? "Nothing shared yet." : "No memory files yet. They appear as this agent learns.")
            }
            ForEach(entries) { file in
                Button { withAnimation(Motion.layout) { target = HqMemoryTarget(shared: file.shared, name: file.name) } } label: {
                    SheetRow(icon: "doc.text", title: file.name, detail: caption(file), chevron: true)
                }
                .buttonStyle(.plain)
            }
        } else if store.listError == nil {
            SheetNote(text: "Loading memory…")
        }
    }

    private func caption(_ file: HqMemoryFileEntry) -> String {
        // `user.md` and `MEMORY.md` are listed before they exist.
        if file.modified == 0 { return "Not created yet" }
        return "\(kithBytes(file.size)) · \(Date(milliseconds: file.modified).formatted(.relative(presentation: .named)))"
    }

    // MARK: tidy-ups

    @ViewBuilder private func tidyRows(_ store: HqMemoryStore) -> some View {
        if let error = store.tidyError {
            SheetNote(text: error, danger: true)
        }
        if let runs = store.tidyUps {
            if runs.isEmpty {
                SheetNote(text: "Tidy-ups appear after you chat with this agent and it has been idle for a few minutes.")
            }
            ForEach(runs) { run in
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: "wand.and.stars").font(.system(size: 13)).foregroundStyle(Palette.muted).frame(width: 18)
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Tidy · \(Date(milliseconds: run.at).formatted(.relative(presentation: .named)))\(run.undone ? " · undone" : "")")
                            .font(.system(size: 13.5, weight: .medium)).foregroundStyle(Palette.ink)
                        ForEach(Array(run.changes.enumerated()), id: \.offset) { _, change in changeRow(change) }
                        ForEach(store.refusals[run.id] ?? [], id: \.file) { refusal in
                            Text("Left alone: \(refusal.file). \(refusal.reason)")
                                .font(.footnote).foregroundStyle(Palette.attention)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    if !run.undone {
                        Button("Undo") { undoing = run }
                            .buttonStyle(.plain)
                            .font(.system(size: 12.5, weight: .semibold))
                            .foregroundStyle(Palette.accent)
                            .disabled(store.busy)
                    }
                }
            }
        } else if store.tidyError == nil {
            SheetNote(text: "Loading memory…")
        }
    }

    private func changeRow(_ change: HqTidyUp.Change) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("\(verb(change.kind)) · \(change.file)").font(.caption).foregroundStyle(Palette.secondary)
            if let was = change.was, !was.isEmpty { Text(was).font(.footnote).foregroundStyle(Palette.tertiary).strikethrough() }
            Text(change.text).font(.footnote)
                .foregroundStyle(change.kind == "archived" ? Palette.tertiary : Palette.text)
        }
    }

    private func verb(_ kind: String) -> String {
        switch kind {
        case "added": "Added"
        case "archived": "Archived"
        case "reviewed": "Reviewed"
        case "noted": "Noted"
        case "indexed": "Indexed"
        default: "Changed"
        }
    }
}

/// One memory file: read it, edit and save it, see who changed it when, and go back to an earlier version.
struct HqMemoryFileView: View {
    let store: HqMemoryStore
    let target: HqMemoryTarget
    @Environment(\.dismissModal) private var dismiss
    @State private var file: HqMemoryText?
    @State private var chunks: [KithMemoryChunks.Chunk] = []
    @State private var versions: [HqMemoryVersion] = []
    /// The newest version when the file was opened: a save over a newer one asks first.
    @State private var openedVersion: Int64?
    @State private var loadError: String?
    @State private var editing = false
    @State private var draft = ""
    @State private var warnings: [String] = []
    @State private var diffs: [Int64: String] = [:]
    @State private var diffing: Int64?
    /// A version to go back to; `undo`: the one before the change it undoes.
    @State private var restoring: (version: HqMemoryVersion, undo: HqMemoryVersion?, later: Int)?
    @State private var confirmDiscard = false
    @State private var moved = false
    @State private var confirmDelete = false

    private var dirty: Bool { editing && draft != (file?.text ?? "") }
    private var canEdit: Bool { file.map { !$0.truncated } ?? false }

    var body: some View {
        VStack(spacing: 0) {
            ModalHeader(target.name) { actions }
            ScrollViewReader { proxy in
            CardForm {
                if let error = loadError ?? store.writeError {
                    CardSection { Text(error).foregroundStyle(Palette.danger).textSelection(.enabled) }
                }
                if !warnings.isEmpty {
                    CardSection("Saved, with a note") {
                        ForEach(warnings, id: \.self) { Text($0).foregroundStyle(Palette.attention) }
                    }
                }
                if let file {
                    content(file)
                    history
                } else if loadError == nil {
                    CardSection { Text("Loading…").foregroundStyle(Palette.secondary) }
                }
            }
            // Lazy rows need a moment before they can be scrolled to.
            .task(id: file?.text) {
                guard let line = target.line, !editing, let index = KithMemoryChunks.index(of: line, in: chunks) else { return }
                try? await Task.sleep(for: .milliseconds(150))
                proxy.scrollTo(index, anchor: .top)
            }
            }
        }
        .background(Palette.background)
        // The X asks before discarding unsaved changes; swiping the sheet down is blocked while there are any.
        .environment(\.dismissModal, DismissModalAction { if dirty { confirmDiscard = true } else { dismiss() } })
        .interactiveDismissDisabled(dirty)
        .task { await load() }
        .kithDialog("Discard your changes?", isPresented: $confirmDiscard) {
            [DialogAction("Discard", destructive: true) {
                // Not dirty any more, and closed once the dialog has finished going away.
                editing = false
                Task {
                    try? await Task.sleep(for: .milliseconds(350))
                    dismiss()
                }
            }]
        }
        .kithDialog("Delete \(target.name)?", isPresented: $confirmDelete,
                      message: "The agent stops seeing it. The deletion is kept as a version, so History can bring it back.") {
            [DialogAction("Delete", destructive: true) {
                Task { if await store.delete(shared: target.shared, name: target.name) != nil { dismiss() } }
            }]
        }
        .kithDialog("This file changed since you opened it", isPresented: $moved,
                      message: "The agent or a tidy-up wrote a newer version. Reload to see it (your edit is dropped), or save over it; the newer version stays in History.") {
            [DialogAction("Reload") { Task { await reload() } },
             DialogAction("Save anyway", destructive: true) { Task { await save(force: true) } }]
        }
        .kithDialog(restoring?.undo != nil ? "Undo this change?" : "Restore this version?",
                      isPresented: Binding(get: { restoring != nil }, set: { if !$0 { restoring = nil } }),
                      message: restoring?.undo != nil
                          ? "The file goes back to how it was before this change, as a new entry in History."
                              + ((restoring?.later ?? 0) > 0
                                 ? " This also removes the \(restoring?.later ?? 0) later change\(restoring?.later == 1 ? "" : "s")."
                                 : "")
                          : "The file becomes how it was after this change, as a new entry in History.") {
            [DialogAction(restoring?.undo != nil ? "Undo this change" : "Restore this version") {
                if let pick = restoring { Task { await restore(pick.undo ?? pick.version) } }
            }]
        }
    }

    @ViewBuilder private var actions: some View {
        if store.busy {
            Spinner()
        } else if editing {
            IconButton("Save", systemImage: "checkmark") { Task { await save(force: false) } }
                .disabled(!dirty)
        } else if canEdit {
            HStack(spacing: 2) {
                IconButton("Edit", systemImage: "pencil") {
                    draft = file?.text ?? ""
                    warnings = []
                    withAnimation(Motion.layout) { editing = true }
                }
                // MEMORY.md is the agent's index: the host keeps it.
                if file?.exists == true && !(!target.shared && target.name == "MEMORY.md") {
                    IconButton("Delete", systemImage: "trash") { withAnimation(Motion.layout) { confirmDelete = true } }
                }
            }
        }
    }

    @ViewBuilder private func content(_ file: HqMemoryText) -> some View {
        if editing {
            CardSection(footer: file.exists ? nil : "This file doesn't exist yet. Saving creates it.") {
                TextEditor(text: $draft)
                    .font(.footnote.monospaced())
                    .scrollContentBackground(.hidden)
                    .frame(minHeight: 320)
                    .disabled(store.busy)
            }
        } else {
            CardSection(footer: file.truncated ? "Larger than 512 KB, so only the start is shown and it can't be edited here." : nil) {
                if file.text.isEmpty {
                    Text(file.exists ? "This file is empty." : "This file doesn't exist yet.").foregroundStyle(Palette.secondary)
                } else {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(chunks.enumerated()), id: \.offset) { index, chunk in
                            Text(chunk.text).font(.footnote.monospaced())
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(target.line.flatMap { KithMemoryChunks.index(of: $0, in: chunks) } == index ? Palette.accentDim : .clear)
                                .id(index)
                        }
                    }
                    .textSelection(.enabled)
                }
            }
        }
    }

    // MARK: history

    @ViewBuilder private var history: some View {
        CardSection("History", footer: versions.count <= 1 ? "Changes are recorded from now on." : nil) {
            ForEach(Array(versions.enumerated()), id: \.element.id) { index, version in
                VStack(alignment: .leading, spacing: 6) {
                    HStack(alignment: .top, spacing: 8) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(describe(version))
                            Text("\(Date(milliseconds: version.at).formatted(.relative(presentation: .named))) · \(version.lines) lines\(index == 0 ? " · current" : "")")
                                .font(.caption).foregroundStyle(Palette.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        if index + 1 < versions.count {
                            IconButton(diffs[version.id] == nil ? "Show what changed" : "Hide what changed", systemImage: "plusminus") {
                                Task { await toggleDiff(version, previous: versions[index + 1]) }
                            }
                            .disabled(diffing != nil)
                        }
                        if index > 0 && !editing {
                            IconButton("Restore this version (the file as it was after this change)", systemImage: "clock.arrow.circlepath") {
                                restoring = (version, nil, 0)
                            }
                            .disabled(store.busy)
                        }
                        if index + 1 < versions.count && !editing && !version.deleted {
                            IconButton("Undo this change", systemImage: "arrow.uturn.backward") {
                                restoring = (version, versions[index + 1], index)
                            }
                            .disabled(store.busy)
                        }
                    }
                    if let diff = diffs[version.id] { diffView(diff) }
                }
                .padding(.vertical, 4)
            }
        }
    }

    private func diffView(_ diff: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            if diff.isEmpty { Text("No difference.").font(.footnote).foregroundStyle(Palette.secondary) }
            ForEach(Array(diff.split(separator: "\n", omittingEmptySubsequences: false).enumerated()), id: \.offset) { _, line in
                Text(String(line))
                    .font(.caption.monospaced())
                    .foregroundStyle(line.hasPrefix("+ ") ? Palette.accent : line.hasPrefix("- ") ? Palette.danger : Palette.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(8)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .textSelection(.enabled)
    }

    private func describe(_ version: HqMemoryVersion) -> String {
        let who = switch version.author {
        case "kai": "You"
        case "agent": "The agent"
        case "tidy": "A tidy-up"
        default: "Another program"
        }
        return switch version.cause {
        case "delete": "\(who) deleted it"
        case "restore": "\(who) restored an earlier version"
        case "undo": "\(who) undid a tidy"
        case "tidy": "\(who) tidied it"
        case "baseline": "First version Kith saw"
        default: "\(who) edited it"
        }
    }

    // MARK: actions

    private func load() async {
        do {
            // History first: the newest version it names can't be newer than the text read after it.
            let past = try await store.history(shared: target.shared, name: target.name)
            let text = try await store.read(shared: target.shared, name: target.name)
            file = text
            chunks = KithMemoryChunks.split(text.text, raw: true)
            versions = past
            openedVersion = past.first?.id
            loadError = nil
            // A file that isn't made yet opens ready to write.
            if !text.exists && !text.truncated && !editing { editing = true }
        } catch {
            loadError = error.localizedDescription
        }
    }

    /// Drops the edit and shows what is on disk now.
    private func reload() async {
        editing = false
        draft = ""
        diffs = [:]
        await load()
    }

    private func save(force: Bool) async {
        guard !store.busy else { return }
        if !force {
            do {
                let latest = try await store.history(shared: target.shared, name: target.name).first?.id
                if latest != openedVersion { moved = true; return }
            } catch {
                loadError = error.localizedDescription
                return
            }
        }
        guard let result = await store.save(shared: target.shared, name: target.name, text: draft) else { return }
        // Warnings never block a save; they are shown after it.
        let note = result.warnings
        withAnimation(Motion.layout) { editing = false }
        await reload()
        warnings = note
    }

    private func restore(_ version: HqMemoryVersion) async {
        guard let result = await store.restore(shared: target.shared, name: target.name, version: version.id) else { return }
        let note = result.warnings
        await reload()
        warnings = note
    }

    private func toggleDiff(_ version: HqMemoryVersion, previous: HqMemoryVersion) async {
        if diffs[version.id] != nil { diffs[version.id] = nil; return }
        diffing = version.id
        defer { diffing = nil }
        do {
            diffs[version.id] = try await store.diff(shared: target.shared, name: target.name, from: previous.id, to: version.id)
        } catch {
            loadError = error.localizedDescription
        }
    }
}
