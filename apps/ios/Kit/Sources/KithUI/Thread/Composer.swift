import KithKit
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// The message box under a chat or a thread: send, or stop while the bot (or the
/// group) is working there. In a group, typing `@` suggests its bots; with one bot, a
/// leading `/` lists its commands.
struct Composer: View {
    let botId: String
    /// Replies go to the thread on this message.
    var thread: String?
    /// Starts a voice call; while the box is empty it takes the send button's place.
    var onCall: (() -> Void)?
    /// Interrupts the bot while it is speaking in a voice call.
    var onInterrupt: (() -> Void)?
    /// Every message starts a new task: set while a terminal holds the current thread, so nothing sent here lands in it.
    var alwaysNewTask = false
    @Environment(BotStore.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// The field owns its text; the store only keeps a copy. A field bound straight to the
    /// store loses a hardware keyboard's IME composition (Zhuyin, Pinyin, Kana) on iPad.
    @State private var draft = ""
    /// Files going out with the next message.
    @State private var files: [OutgoingFile] = []
    @State private var pickingFiles = false
    @State private var pickingPhotos = false
    @State private var pickingCamera = false
    @State private var photoItems: [PhotosPickerItem] = []
    @FocusState private var focused: Bool
    @State private var dictation = Dictation()
    /// The box when dictation started; the words heard go after it.
    @State private var dictatedAfter = ""

    /// The next message starts a task of its own.
    @State private var newTask = false
    /// Where the new task starts, when picked up front; nil follows the bot's default.
    @State private var startFolder: FolderChoice?
    @State private var pickingFolder = false

    private var bot: Bot? { model.bots[botId] }
    private var tasks: Bool { thread == nil && bot?.usesTasks == true }
    /// In a bot's chat a message goes on with the current task: Stop and "working" are that task's.
    private var place: String? { thread ?? (tasks ? Tasks.current(in: model.chat(botId))?.id : nil) }
    /// The next message starts a task of its own: asked for, or the reader just left a thread (see `BotStore.leftThread`).
    private var startsNew: Bool {
        alwaysNewTask || model.startsNewTask(botId, thread: thread, requested: newTask, currentOpen: tasks ? model.isCurrentTaskOpen(botId) : nil)
    }
    /// The chip is for a task asked for or one reached by leaving a thread; a folded task just changes the placeholder.
    private var showsNewTaskChip: Bool { startsNew && (alwaysNewTask || newTask || model.leftThread.contains(botId)) }
    private var working: Bool { !startsNew && bot?.isWorking(in: botId, thread: place) == true }

    private var isEmpty: Bool { draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && files.isEmpty }

    private var canSend: Bool {
        // Offline computers still take messages when the relay can hold them for it (text only).
        // A starting folder can't wait in the relay mailbox.
        !isEmpty && (model.connection == .online || (model.canQueue && files.isEmpty && startFolder == nil))
    }

    private var canAttach: Bool { bot != nil }

    private var placeholder: String {
        let name = bot?.name ?? ""
        if thread != nil { return "Reply…" }
        if bot?.isGroup == true { return "Message everyone · @ to ask one bot" }
        if alwaysNewTask { return "New task…" }
        if startsNew { return "Tell \(name)…" }
        if working { return "Queue a message for \(name)" }
        return "Tell \(name)…"
    }

    /// The `@partial` being typed at the end, if any.
    private var mentionQuery: String? {
        guard bot?.isGroup == true, let at = draft.lastIndex(of: "@") else { return nil }
        let query = draft[draft.index(after: at)...]
        if at > draft.startIndex, !draft[draft.index(before: at)].isWhitespace { return nil }
        return query.contains(where: \.isNewline) || query.count > 24 ? nil : String(query)
    }

    private var suggestions: [Bot] {
        guard let query = mentionQuery, let bot else { return [] }
        return model.members(of: bot).filter {
            query.isEmpty || $0.name.localizedCaseInsensitiveContains(query)
        }
    }

    /// The bot's `/` commands while the draft is `/partial`.
    private var commands: [SlashCommand] {
        guard let query = SlashCommands.query(draft) else { return [] }
        return SlashCommands.list(for: bot, query: query, skills: model.installedSkills)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !commands.isEmpty {
                CommandList(commands: commands, pick: run)
                    .padding(.horizontal, 16)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            if !suggestions.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(suggestions) { member in
                            SuggestionChip(title: member.name) {
                                CharacterAvatar(bot: member, size: 18, animated: false)
                            } action: {
                                mention(member)
                            }
                        }
                    }
                    .padding(.horizontal, 16)
                }
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            dock
        }
        .animation(Motion.layout, value: suggestions.map(\.id))
        .animation(Motion.layout, value: commands.map(\.id))
    }

    /// The chat box: the redesign's `ChatDock`, with this composer's features in its slots and bindings.
    private var dock: some View {
        ChatDock(text: $draft, placeholder: placeholder, showsCall: onCall != nil, hasAttachment: !files.isEmpty,
                 onSend: submit, onMic: toggleDictation,
                 onCall: {
                     // The call takes the microphone.
                     dictation.stop()
                     onCall?()
                 },
                 onPhotos: { pickingPhotos = true },
                 onCamera: {
                     if ChatCameraPicker.isAvailable { pickingCamera = true } else { model.lastError = "This device has no camera." }
                 },
                 onFiles: { pickingFiles = true },
                 extraActions: extraActions, micListening: dictation.listening, micWithText: true, canSend: canSend,
                 showsPlus: canAttach, stop: stop, focus: $focused) {
            accessory
        }
        .animation(Motion.layout, value: files.map(\.id))
        .animation(Motion.layout, value: startsNew)
        .dropDestination(for: PickedFile.self) { picked, _ in
            guard canAttach else { return false }
            append(picked.map(\.file))
            return true
        }
        .fileImporter(isPresented: $pickingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            if case .success(let urls) = result { add(urls) }
        }
        .photosPicker(isPresented: $pickingPhotos, selection: $photoItems, maxSelectionCount: 10, matching: .images)
        // Replying to a message puts the cursor in the box.
        .onChange(of: model.replyQuotes[botId]?.id) { _, id in if id != nil { focused = true } }
        .onChange(of: photoItems) { _, items in
            guard !items.isEmpty else { return }
            photoItems = []
            Task { await addPhotos(items) }
        }
        .fullScreenCover(isPresented: $pickingCamera) {
            ChatCameraPicker { data in append(HomeAttachments.photos([data])) }.ignoresSafeArea()
        }
        .padding(.top, 6)
        .padding(.bottom, 8)
        .onChange(of: [botId, thread], initial: true) {
            dictation.stop()
            draft = model.composerDraft(for: botId, thread: thread)
        }
        .onDisappear { dictation.stop() }
        .onChange(of: draft) { _, value in
            model.setComposerDraft(value, for: botId, thread: thread)
            // Starting a command: list the computer's skills again.
            if value == "/", bot?.isGroup == false { Task { await model.refreshPlugins() } }
        }
        .onChange(of: model.routineDrafts[botId]) { _, value in
            guard thread == nil, let value else { return }
            draft = draft.isEmpty ? value : draft + "\n" + value
            model.routineDrafts.removeValue(forKey: botId)
            focused = true
        }
    }

    /// Paste and New task join Photos / Camera / Files in the + menu.
    private var extraActions: [ChatDockAction] {
        guard canAttach else { return [] }
        var items: [ChatDockAction] = []
        if UIPasteboard.general.hasImages || UIPasteboard.general.hasURLs {
            items.append(ChatDockAction("Paste", systemImage: "doc.on.clipboard") { pasteFiles() })
        }
        if tasks && !startsNew {
            items.append(ChatDockAction("New task", systemImage: "square.and.pencil") {
                newTask = true
                focused = true
            })
        }
        return items
    }

    /// Above the field, inside the dock: the new-task chip and the files going out.
    private var accessory: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let quote {
                quoteChip(quote).transition(.move(edge: .bottom).combined(with: .opacity))
            }
            if showsNewTaskChip {
                newTaskChip.transition(.move(edge: .bottom).combined(with: .opacity))
            }
            if !files.isEmpty {
                fileChips.transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
    }

    /// Beside the mic while the box is empty: cut the voice off in a call, or stop the bot's work here.
    private var stop: ChatDockStop? {
        if let onInterrupt { return ChatDockStop("Interrupt", urgent: true, action: onInterrupt) }
        if working { return ChatDockStop("Stop") { model.stop(botId, thread: place) } }
        return nil
    }

    /// The mic starts recording; while it records the same slot is its Stop, which leaves the words to edit.
    private func toggleDictation() {
        if dictation.listening {
            dictation.stop()
            focused = true
        } else {
            dictatedAfter = draft
            Task { await dictation.start { said in draft = Dictation.join(dictatedAfter, said) } }
        }
    }

    /// The line being replied to (main chat only; a thread has its own box).
    private var quote: String? { thread == nil ? model.replyQuotes[botId].flatMap(ReplyQuote.line(of:)) : nil }

    /// Over the box while replying to a message; its x drops the quote.
    private func quoteChip(_ line: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: "arrowshape.turn.up.left").font(.caption).foregroundStyle(Palette.muted)
            Text(line).kithText(.secondary).foregroundStyle(Palette.ink).lineLimit(1)
            Spacer(minLength: 0)
            IconButton("Cancel reply", systemImage: "xmark") { model.replyQuotes[botId] = nil }
        }
        .padding(.leading, 14)
        .padding(.top, 8)
    }

    /// Over the box while the next message starts a new task; its x goes back to the current one.
    private var newTaskChip: some View {
        HStack(spacing: 6) {
            Image(systemName: "square.and.pencil").font(.caption).foregroundStyle(Palette.ink)
            Text("New task").kithText(.secondary).foregroundStyle(Palette.ink)
            Button { pickingFolder = true } label: {
                HStack(spacing: 3) {
                    Image(systemName: startFolder == nil ? "folder" : "folder.fill").font(.caption2)
                    Text(startFolderName).kithText(.secondary).lineLimit(1)
                }
                .foregroundStyle(startFolder == nil ? Palette.muted : Palette.ink)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Starting folder: \(startFolderName)")
            // No way back to the held task from here: the terminal writes it.
            if !alwaysNewTask {
                IconButton("Go on with the current task", systemImage: "xmark") {
                    newTask = false
                    model.leftThread.remove(botId)
                    startFolder = nil
                    // Going on means the current task, even if it was folded: open it again.
                    if let root = Tasks.current(in: model.chat(botId)) { model.taskChoices[root.id] = true }
                }
            }
        }
        .kithSheet(isPresented: $pickingFolder) {
            PlaceFolderPicker(current: nil, offersDefault: startFolder != nil) { startFolder = $0 }
        }
        .padding(.leading, 14)
        .padding(.top, 8)
    }

    private var startFolderName: String {
        switch startFolder {
        case .project(let id): id
        case .folder(let path): (path as NSString).lastPathComponent
        case nil: "Default folder"
        }
    }

    private var fileChips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 6) {
                ForEach(files) { file in
                    ComposerAttachment(file: file) {
                        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) {
                            files.removeAll { $0.id == file.id }
                        }
                    }
                }
            }
            .padding(.horizontal, 10)
            .padding(.top, 8)
        }
    }

    private func add(_ urls: [URL]) {
        append(urls.compactMap { url in
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            return (try? Data(contentsOf: url, options: .mappedIfSafe)).map { OutgoingFile.prepared(name: url.lastPathComponent, data: $0) }
        })
    }

    private func append(_ new: [OutgoingFile]) {
        guard canAttach else { return }
        for file in new {
            guard file.data.count <= OutgoingFile.maxSize else {
                model.lastError = "\(file.name) is larger than 100 MB."
                continue
            }
            files.append(file)
        }
    }

    /// Images (or files) on the clipboard become attachments.
    private func pasteFiles() {
        let providers = UIPasteboard.general.itemProviders
        Task { append(await PickedFile.load(providers)) }
    }

        /// Photos go out as JPEG: HEIC isn't something every agent can read.
        private func addPhotos(_ items: [PhotosPickerItem]) async {
            let stamp = Int(Date.now.timeIntervalSince1970)
            var picked: [OutgoingFile] = []
            for (i, item) in items.enumerated() {
                guard let data = try? await item.loadTransferable(type: Data.self),
                      let jpeg = ImageData.jpeg(data) else { continue }
                picked.append(OutgoingFile(name: "photo-\(stamp)-\(i + 1).jpg", data: jpeg))
            }
            append(picked)
        }

    private func mention(_ member: Bot) {
        guard let at = draft.lastIndex(of: "@") else { return }
        draft = String(draft[..<at]) + "@\(member.name) "
        focused = true
    }

    private func run(_ command: SlashCommand) {
        guard let bot else { return }
        switch command.action {
        case .fast:
            model.setFastMode(bot, !bot.fastMode)
            draft = ""
        case .skill(let id):
            draft = "/\(id) "
        }
        focused = true
    }

    private func submit() {
        // Send while recording: the recording ends and what it heard goes with the message.
        dictation.stop()
        if files.isEmpty, let command = SlashCommands.builtIn(for: bot, draft: draft) { return run(command) }
        guard canSend else { return }
        withAnimation(Motion.reduced(Motion.conversation, reduceMotion)) {
            guard model.sendComposerDraft(to: botId, thread: thread, files: files, newTask: startsNew,
                                           start: startsNew ? startFolder : nil) else { return }
            draft = ""
            files = []
            newTask = false
            model.leftThread.remove(botId)
            startFolder = nil
        }
    }
}

/// One suggestion above the box: a bot to @mention.
private struct SuggestionChip<Icon: View>: View {
    let title: String
    @ViewBuilder let icon: Icon
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                icon
                Text(title).kithText(.secondary).foregroundStyle(Palette.ink)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(Palette.panel, in: Capsule())
        }
        .buttonStyle(PressScale())
    }
}
