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
    /// The task cards on screen: a working task whose card is not gets the "working now" line over the box.
    var visibleTasks: VisibleTasks?
    /// Scrolls the chat to a task's card.
    var jump: ((String) -> Void)?
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

    /// A send waiting on a take-over from the terminal: the box holds still until it is done.
    @State private var taking = false
    /// Where the new task starts, when picked up front; nil follows the bot's default.
    @State private var startFolder: FolderChoice?
    @State private var pickingFolder = false
    /// In a group: the turns this message gives the room. Each message starts at the standard number; it never sticks.
    @State private var turns = Turns.standard

    private var bot: Bot? { model.bots[botId] }
    private var tasks: Bool { thread == nil && bot?.usesTasks == true }
    /// Where the chat's one box sends: the task a Reply picked, else the one the host would go on with; "New task"
    /// starts another. nil outside a bot's main chat.
    private var destination: TaskDestination? { tasks ? model.destination(botId) : nil }
    /// A message goes on with this task: "working" is that task's.
    private var place: String? { thread ?? (tasks ? destination?.into?.id ?? Tasks.current(in: model.chat(botId))?.id : nil) }
    private var startsNew: Bool { destination?.newTask == true }
    private var working: Bool {
        !startsNew && (bot?.isWorking(in: botId, thread: place) == true || (tasks && bot?.isWorking(in: botId, thread: nil) == true))
    }
    /// The run in progress in this room, whose line (with its Stop) sits above the box.
    private var openConversation: RoomConversation? {
        bot?.isGroup == true ? model.roomConversations.open(chat: botId, lane: thread) : nil
    }

    /// Working tasks whose card is off screen, newest first.
    private var workingNow: [Entry] {
        guard tasks, let visibleTasks, let bot else { return [] }
        let lanes = Set((bot.workingLanes ?? []).filter { $0.chat == botId }.compactMap(\.thread))
        guard !lanes.isEmpty else { return [] }
        return model.chat(botId).filter { $0.isTask && lanes.contains($0.id) && !visibleTasks.ids.contains($0.id) }.reversed()
    }

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
        if model.myRooms[botId] != nil { return "Message, or @ an agent" }
        if bot?.isGroup == true { return "Message everyone · @ to ask one bot" }
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
        // An invited person's room has no member bots to mention: the agents it names answer when @'d.
        let pool = model.myRooms[botId].map { room in MemberRoomLogic.agentNames(room).compactMap { name in MemberRoomLogic.bot(for: HostContract.MyRoom(id: "room-agent:\(name)", name: name, agents: [], stay: 0)).map { b -> Bot in var b = b; b.kind = "agent"; return b } } } ?? model.members(of: bot)
        return pool.filter {
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
            if let conversation = openConversation {
                RoomLiveLineView(conversation: conversation) { model.stop(botId, thread: thread) }
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
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
            if tasks, let first = workingNow.first {
                WorkingNowLine(task: first, more: workingNow.count - 1, bot: bot) { jump?(first.id) }
                    .padding(.horizontal, KithSpacing.gutter)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            if showsPills, let destination {
                pills(destination)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            dock
        }
        .animation(Motion.layout, value: suggestions.map(\.id))
        .animation(Motion.reduced(Motion.layout, reduceMotion), value: showsPills)
        .animation(Motion.reduced(Motion.layout, reduceMotion), value: workingNow.map(\.id))
        .animation(Motion.layout, value: commands.map(\.id))
        .animation(Motion.reduced(Motion.layout, reduceMotion), value: openConversation?.conversation)
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
                 showsPlus: canAttach, stop: stop, turns: bot?.isGroup == true && thread == nil ? $turns : nil, focus: $focused) {
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
        // A task's Reply or a take-over picks where the message goes: the cursor follows, as on the Mac.
        .onChange(of: model.taskTargets[botId]) { _, target in if target != nil { focused = true } }
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
        .onAppear { if DemoScreens.name == "room-turns" { turns = 25 } }
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

    /// The pills show while there is something to send, or once a Reply or "New task" picked where it goes.
    private var showsPills: Bool { tasks && (!isEmpty || model.taskTargets[botId] != nil) }

    /// Above the box: where the message goes, one tap to switch. "Continues" stays after picking New task, so the
    /// way back is one tap too.
    private func pills(_ destination: TaskDestination) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 6) {
                if let going = destination.going {
                    let note = Tasks.takesOver(going.data.thread, picked: destination.picked) && !destination.newTask ? " · takes over from the terminal" : ""
                    TaskPill(on: !destination.newTask, systemImage: "arrow.turn.down.right",
                             label: Text("Continues ") + Text(Tasks.title(going)).fontWeight(.semibold) + Text(note)) {
                        model.taskTargets[botId] = .task(going.id)
                        focused = true
                    }
                    .accessibilityLabel("Continues \(Tasks.title(going))\(note)")
                }
                TaskPill(on: destination.newTask || destination.going == nil, systemImage: "plus", label: Text("New task")) {
                    model.taskTargets[botId] = .new
                    focused = true
                }
                if destination.newTask { folderButton }
            }
            .padding(.horizontal, KithSpacing.gutter)
        }
        .kithSheet(isPresented: $pickingFolder) {
            PlaceFolderPicker(current: nil, offersDefault: startFolder != nil) { startFolder = $0 }
        }
    }

    /// Paste joins Photos / Camera / Files in the + menu.
    private var extraActions: [ChatDockAction] {
        guard canAttach else { return [] }
        var items: [ChatDockAction] = []
        if UIPasteboard.general.hasImages || UIPasteboard.general.hasURLs {
            items.append(ChatDockAction("Paste", systemImage: "doc.on.clipboard") { pasteFiles() })
        }
        return items
    }

    /// Above the field, inside the dock: the quoted message and the files going out.
    private var accessory: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let quote {
                quoteChip(quote).transition(.move(edge: .bottom).combined(with: .opacity))
            }
            if !files.isEmpty {
                fileChips.transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
    }

    /// Beside the mic while the box is empty: cut the voice off in a call, or stop the bot's work here.
    private var stop: ChatDockStop? {
        if let onInterrupt { return ChatDockStop("Interrupt", urgent: true, action: onInterrupt) }
        // The live line above the box carries the group's Stop while its run is open.
        // A task's Stop is on its card; a turn in the chat's own lane has no card, so its Stop stays here.
        if working, openConversation == nil, !tasks || bot?.isWorking(in: botId, thread: nil) == true {
            return ChatDockStop("Stop") { model.stop(botId, thread: tasks ? nil : place) }
        }
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

    /// Beside "New task": the folder it starts in.
    private var folderButton: some View {
        Button { pickingFolder = true } label: {
            HStack(spacing: 3) {
                Image(systemName: startFolder == nil ? "folder" : "folder.fill").font(.caption2)
                Text(startFolderName).kithText(.secondary).lineLimit(1)
            }
            .foregroundStyle(startFolder == nil ? Palette.muted : Palette.ink)
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Starting folder: \(startFolderName)")
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
        guard canSend, !taking else { return }
        let destination = destination
        let into = destination?.into
        let newTask = startsNew
        let send = {
            withAnimation(Motion.reduced(Motion.conversation, reduceMotion)) {
                guard model.sendComposerDraft(to: botId, thread: thread, into: into?.id, files: files, newTask: newTask,
                                               start: newTask ? startFolder : nil,
                                               budget: bot?.isGroup == true ? turns : nil) else { return }
                turns = Turns.standard
                draft = ""
                files = []
                startFolder = nil
                // "New task" is for the next message; after it the box goes on with the task it started.
                if newTask { model.taskTargets[botId] = nil }
            }
        }
        guard let into, Tasks.takesOver(into.data.thread, picked: destination?.picked == true) else { return send() }
        // A picked task an idle terminal holds is taken over first; if that fails the text stays here, never sent as a new task.
        taking = true
        Task {
            let failure = await Tasks.sendIn(into.data.thread, picked: true, takeOver: {
                _ = try await model.ready().takeOverThread(botId: botId, threadId: into.id)
            }, send: send)
            taking = false
            if let failure {
                let why = (failure as? ThreadWriterError)?.errorDescription ?? failure.localizedDescription
                model.lastError = "Couldn't take over from the terminal, so nothing was sent (\(why)). Your message is still in the box."
            }
        }
    }
}

/// A pill over the chat's box: where the next message goes.
private struct TaskPill: View {
    let on: Bool
    let systemImage: String
    let label: Text
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Image(systemName: systemImage).font(.caption2.weight(.semibold))
                label.kithText(.secondary).lineLimit(1)
            }
            .foregroundStyle(on ? Palette.background : Palette.ink)
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(on ? Palette.ink : Color.clear, in: Capsule())
            .overlay(Capsule().strokeBorder(on ? Color.clear : Palette.line, lineWidth: 1))
            .contentShape(Capsule())
        }
        .buttonStyle(PressScale())
        .accessibilityAddTraits(on ? .isSelected : [])
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
