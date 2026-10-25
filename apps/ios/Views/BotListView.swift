import KithKit
import KithUI
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// Crew, the home screen: every room on every computer as one list under the morning sky, a top bar (your picture,
/// the Crew switch, new chat) and the dock at the bottom, where typing filters the list in place.
/// A `List` with its styling taken away, for the system's reordering: touch and hold opens a room's menu, and
/// dragging it (before or after the menu shows) moves it, where the order setting allows.
struct BotListView: View {
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts
    @State private var editing: EditTarget?
    @State private var editingGroup: GroupTarget?
    @State private var confirmDelete: RosterItem?
    @State private var newChat: ChatTarget?
    @State private var inviting: ChatTarget?
    @State private var botSheet: BotSheetTarget?
    @State private var sharingAgent: ShareAgentTarget?
    fileprivate struct ShareAgentTarget: Identifiable {
        let store: BotStore
        let agent: String
        let name: String
        let id: String
        @MainActor init(store: BotStore, agent: String, name: String) {
            self.store = store
            self.agent = agent
            self.name = name
            id = "\(store.computer.id)/\(agent)"
        }
    }
    fileprivate struct ChatTarget: Identifiable {
        let store: BotStore
        let id: ComputerID
        @MainActor init(store: BotStore) {
            self.store = store
            id = store.computer.id
        }
    }
    /// The room whose row is lifted, to drag or for its menu.
    @State private var lifted: String?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// Computers left out of the list on this device, comma-separated IDs.
    @AppStorage("hiddenComputers") private var hiddenComputers = ""
    @AppStorage(CrewOrderMode.storageKey) private var orderStored = CrewOrderMode.automatic.rawValue

    // Crew and Board are two pages of one screen: the same sky, top bar and dock.
    @State private var tab = ["board", "bots-board", "keys-board"].contains(DemoScreens.name) ? CrewBoardTab.board : CrewBoardTab.crew
    @State private var pageWidth: CGFloat = 390
    // The dock.
    @State private var query = DemoScreens.query
    @State private var files: [OutgoingFile] = []
    @State private var pickingPhotos = false
    @State private var photoItems: [PhotosPickerItem] = []
    @State private var pickingFiles = false
    @State private var pickingCamera = false
    @State private var picking: AgentPick?
    @State private var calling: CallTarget?
    @State private var dictation = HomeDictation()

    // Never re-sort while a finger is on the list.
    @GestureState private var touching = false
    @State private var scrolling = false
    @State private var hold = CrewOrderHold()

    fileprivate struct CallTarget: Identifiable {
        let store: BotStore
        let botId: String
        let id: String
        @MainActor init(store: BotStore, botId: String) {
            self.store = store
            self.botId = botId
            id = "\(store.computer.id)/\(botId)"
        }
    }

    private var mode: CrewOrderMode { CrewOrderMode(stored: orderStored) }

    private var shownIDs: Set<ComputerID> {
        Set(ComputerSelection(all: accounts.computers.map(\.id), hidden: hiddenComputers).shown)
    }

    private var shownStores: [BotStore] {
        accounts.computers.filter { shownIDs.contains($0.id) }.compactMap { accounts.store(for: $0.id) }
    }

    /// Computers that can take a new bot right now.
    private var onlineStores: [BotStore] {
        allStores.filter { $0.connection == .online && $0.mismatch == nil }
    }

    /// Computers this account owns: new bots and groups are made there. A computer joined by invite
    /// takes `New chat` instead, since an invited person's chat is made by the computer.
    private var allStores: [BotStore] {
        accounts.ownedComputers.compactMap { accounts.store(for: $0.id) }
    }

    private var memberStores: [BotStore] {
        accounts.computers.filter { accounts.role(for: $0.id) == .member }
            .compactMap { accounts.store(for: $0.id) }.filter { $0.connection == .online }
    }

    // MARK: rooms

    private struct Room: Identifiable {
        let item: RosterItem
        let store: BotStore
        let team: Bool
        var id: String { CrewRows.rowId(item) }
    }

    /// Every shown computer's rooms, together as one crew.
    private var rooms: [Room] {
        accounts.roster.filter { shownIDs.contains($0.ref.computerId) }.compactMap { item in
            accounts.store(for: item.ref.computerId).map {
                Room(item: item, store: $0, team: accounts.role(for: item.ref.computerId) == .member)
            }
        }
    }

    /// Where this computer sits in the order kept on this iPhone.
    private func computerIndex(_ id: ComputerID) -> Int { accounts.computers.firstIndex { $0.id == id } ?? 0 }

    private var frozen: Bool { touching || scrolling || lifted != nil }

    private var computedOrder: [String] { CrewOrder.sorted(rooms.map { CrewRows.orderItem($0.item, computerIndex: computerIndex($0.item.ref.computerId)) }, mode: mode) }

    private var trimmedQuery: String { query.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var searching: Bool { !trimmedQuery.isEmpty }

    /// The rooms as shown: the crew's order, or the dock's ranking while something is typed.
    private func displayed(_ all: [Room]) -> [Room] {
        let byId = Dictionary(all.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let ordered = hold.resolved(computed: computedOrder, frozen: frozen).compactMap { byId[$0] }
        guard searching else { return ordered }
        let entries = ordered.map { CrewRows.searchEntry($0.item, store: $0.store, team: $0.team) }
        return CrewSearch.rank(entries, query: trimmedQuery).compactMap { byId[$0] }
    }

    // MARK: body

    var body: some View {
        let all = rooms
        let shown = displayed(all)
        let pinned = searching ? [] : shown.filter { $0.item.bot.pinned }
        let rest = searching ? shown : shown.filter { !$0.item.bot.pinned }
        let board = boardModel
        let shownBoard = board.filtered(by: query)
        ZStack(alignment: .top) {
            SkyBackground(.home)
            ZStack(alignment: .top) {
                crewList(all: all, shown: shown, pinned: pinned, rest: rest)
                    .offset(x: tab == .crew ? 0 : -pageWidth)
                    .opacity(tab == .crew ? 1 : 0)
                    .allowsHitTesting(tab == .crew)
                    .accessibilityHidden(tab != .crew)
                BoardView(model: shownBoard, searching: searching, showsHosts: accounts.computers.count > 1,
                          store: { accounts.store(for: $0) }, open: openBoard, showComputers: { app.showComputers = true })
                    .offset(x: tab == .board ? 0 : pageWidth)
                    .opacity(tab == .board ? 1 : 0)
                    .allowsHitTesting(tab == .board)
                    .accessibilityHidden(tab != .board)
            }
            .background(GeometryReader { Color.clear.preference(key: PageWidthKey.self, value: $0.size.width) })
            .onPreferenceChange(PageWidthKey.self) { pageWidth = $0 }
            .animation(Motion.reduced(.snappy(duration: 0.25), reduceMotion), value: tab)
            .simultaneousGesture(swipe)
        }
        .safeAreaInset(edge: .top, spacing: 0) { topBar(waiting: board.waiting) }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            VStack(spacing: 8) {
                AccessBanners()
                if tab == .crew {
                    dock(leading: leads(!shown.isEmpty)) { submit(shown) }
                } else {
                    dock(leading: leads(shownBoard.top != nil)) { submitBoard(shownBoard) }
                }
            }
            .padding(.bottom, 4)
            // Swallow taps in the dock's gaps so they never reach a room scrolling beneath it.
            .background(Color.clear.contentShape(Rectangle()).onTapGesture {})
        }
        .toolbar(.hidden, for: .navigationBar)
        .onChange(of: computedOrder, initial: true) { _, _ in syncOrder() }
        .onChange(of: frozen) { _, _ in syncOrder() }
        .modifier(HomeSheets(
            editing: $editing, editingGroup: $editingGroup, confirmDelete: $confirmDelete, newChat: $newChat,
            inviting: $inviting, botSheet: $botSheet, sharingAgent: $sharingAgent, allStores: allStores, query: $query, files: $files, picking: $picking, calling: $calling,
            pickingPhotos: $pickingPhotos, photoItems: $photoItems, pickingFiles: $pickingFiles, pickingCamera: $pickingCamera,
            pickChoices: pickChoices(for:), addPhotos: addPhotos, addFiles: addFiles, addCamera: { append(HomeAttachments.photos([$0])) }))
    }

    /// Everything the Board derives from, across the shown computers.
    private var boardModel: BoardModel {
        BoardModel(computers: shownStores.map { BoardComputer(store: $0, guestPrompts: accounts.guestPrompts(for: $0.computer.id),
                                              botRequests: accounts.botRequests(for: $0.computer.id)) })
    }

    /// A sideways swipe moves between the pages; the top bar's switch does the same.
    private var swipe: some Gesture {
        DragGesture(minimumDistance: 30).onEnded { drag in
            let across = drag.translation.width, down = drag.translation.height
            guard abs(across) > 60, abs(across) > abs(down) * 1.5 else { return }
            if across < 0, tab == .crew { tab = .board } else if across > 0, tab == .board { tab = .crew }
        }
    }

    private func crewList(all: [Room], shown: [Room], pinned: [Room], rest: [Room]) -> some View {
        List {
            Group {
                if !searching {
                    ForEach(shownStores, id: \.computer.id) { updateCard($0) }
                    machineChips
                    // A computer waiting for an update has its card instead; its bots aren't known yet.
                    if all.isEmpty && shownStores.allSatisfy({ $0.mismatch == nil }) {
                        EmptyRoster(canCreate: allStores.contains { $0.mismatch == nil }, hasComputer: !accounts.computers.isEmpty,
                                    create: newBot, showComputers: { app.showComputers = true })
                    }
                    needsYourOK
                }
                if searching && shown.isEmpty {
                    Text("No match. Send starts a new session with an agent you pick.")
                        .kithText(.body).foregroundStyle(Palette.muted)
                        .padding(.horizontal, KithSpacing.gutter).padding(.vertical, 18)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                ForEach(pinned) { row($0, in: pinned) }
                    .onMove(perform: movePinned(pinned))
                ForEach(rest) { row($0, in: rest) }
                    .onMove(perform: searching || mode == .automatic ? nil : moveRest(pinned: pinned, rest: rest))
            }
            .listRowInsets(EdgeInsets())
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .environment(\.defaultMinListRowHeight, 0)
        .scrollDismissesKeyboard(.immediately)
        .onDisappear { dictation.stop() }
        .onScrollPhaseChange { _, phase in scrolling = phase != .idle }
        .simultaneousGesture(DragGesture(minimumDistance: 0).updating($touching) { _, state, _ in state = true })
        .refreshable {
            for computer in accounts.computers { accounts.store(for: computer.id)?.restartStream() }
            await accounts.refreshCloud()
        }
    }

    private func syncOrder() {
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { hold.update(computed: computedOrder, frozen: frozen) }
    }

    // MARK: top bar

    /// `waiting` is every question waiting on you, not just the ones the dock's box leaves showing.
    private func topBar(waiting: Int) -> some View {
        HStack {
            ProfileMenu()
            Spacer(minLength: 8)
            CrewBoardSwitch(selection: $tab, boardCount: waiting)
            Spacer(minLength: 8)
            TopGlassMenu("New chat") {
                Button("New agent", systemImage: "plus.square", action: newBot)
                    .disabled(!allStores.contains { $0.mismatch == nil })
                Button("Invite a person", systemImage: "person.badge.plus") {
                    if let store = onlineStores.first { inviting = ChatTarget(store: store) }
                }
                .disabled(onlineStores.isEmpty)
                if !(onlineStores.first?.botGuests.unsupported ?? false) {
                    Button("Invite a bot", systemImage: "bubble.left.and.bubble.right") {
                        if let store = onlineStores.first { botSheet = BotSheetTarget.invite(on: store) }
                    }
                    .disabled(onlineStores.isEmpty)
                }
                Button("New group chat", systemImage: "person.2", action: newGroup)
                    .disabled(onlineStores.isEmpty)
                ForEach(memberStores, id: \.computer.id) { store in
                    Button("New chat on \(store.hostName)", systemImage: "square.and.pencil") { newChat = ChatTarget(store: store) }
                }
            }
            // A computer that needs an update can't take a new bot.
            .disabled(!allStores.contains { $0.mismatch == nil } && memberStores.isEmpty)
        }
        .padding(.horizontal, KithSpacing.gutter)
        .padding(.top, 4)
    }

    // MARK: rows

    private func row(_ room: Room, in section: [Room]) -> some View {
        let item = room.item, store = room.store, bot = item.bot
        let reorderable = !searching && CrewOrder.canReorder(pinned: bot.pinned, mode: mode)
        let card = RoundedRectangle(cornerRadius: 16, style: .continuous)
        // A plain button instead of a NavigationLink: same push, no chevron.
        return Button { open(item.ref) } label: {
            CrewRow(CrewRows.model(for: item, store: store, team: room.team))
        }
        .buttonStyle(.plain)
        .background(lifted == room.id ? Palette.surface : .clear, in: card)
        .contentShape([.dragPreview, .contextMenuPreview], card)
        .modifier(LiftIfReorderable(reorderable: reorderable) { lift(room.id) })
        .alignmentGuide(.listRowSeparatorLeading) { _ in KithSpacing.gutter + 54 + 14 }
        .listRowSeparator(.visible)
        .listRowSeparatorTint(Palette.ink.opacity(0.08))
        .accessibilityLabel(accessibilityName(room))
        .accessibilityActions {
            if reorderable {
                Button("Move up") { shift(room, by: -1, in: section) }
                Button("Move down") { shift(room, by: 1, in: section) }
            }
        }
        .contextMenu { menu(item, store: store) }
    }

    private func accessibilityName(_ room: Room) -> String {
        accounts.computers.count > 1 ? "\(room.item.bot.name), on \(room.store.hostName)" : room.item.bot.name
    }

    @ViewBuilder private func menu(_ item: RosterItem, store: BotStore) -> some View {
        if item.bot.isReadOnly {
            readOnlyMenu(item, store: store)
        } else {
            fullMenu(item, store: store)
        }
    }

    /// A bot from outside's chat is read-only. Keeping it tidy is the computer owner's (a member owner has no say on the row).
    @ViewBuilder private func readOnlyMenu(_ item: RosterItem, store: BotStore) -> some View {
        let bot = item.bot
        if accounts.role(for: item.ref.computerId) == .owner {
            Button("Mark as read", systemImage: "checkmark.message") { store.markAllRead(bot.id) }
            Button("Hide from list", systemImage: "eye.slash") { store.setHidden(bot, true) }
        }
    }

    @ViewBuilder private func fullMenu(_ item: RosterItem, store: BotStore) -> some View {
        let bot = item.bot
        Button(bot.pinned ? "Unpin" : "Pin", systemImage: bot.pinned ? "pin.slash" : "pin") { store.setPinned(bot, !bot.pinned) }
        Menu("Colour", systemImage: "paintpalette") {
            ForEach(AvatarPalette.colors) { swatch in
                Button { store.setAvatarColor(bot, swatch.stored) } label: {
                    Label(swatch.label, systemImage: AvatarPalette.pick(bot.avatarColor) == swatch.id ? "checkmark" : "circle.fill")
                }
            }
        }
        if bot.isGroup {
            Button("Edit group", systemImage: "person.2") { editingGroup = GroupTarget(computerId: item.ref.computerId, group: bot) }
        } else {
            Button("Edit profile", systemImage: "pencil") { editing = EditTarget(computerId: item.ref.computerId, draft: BotDraft(bot)) }
        }
        if accounts.role(for: item.ref.computerId) == .owner, store.connection == .online, !bot.isGroup,
           let agent = bot.hqAgent, AgentShare.canShare(hqAgent: agent) {
            Button("Share access…", systemImage: "person.badge.plus") { sharingAgent = ShareAgentTarget(store: store, agent: agent, name: bot.name) }
        }
        Button("Mark as read", systemImage: "checkmark.message") { store.markAllRead(bot.id) }
        Button("Hide from list", systemImage: "eye.slash") { store.setHidden(bot, true) }
        Divider()
        Button("Delete", systemImage: "trash", role: .destructive) { confirmDelete = item }
    }

    private func open(_ ref: BotReference) {
        dictation.stop()
        query = ""
        accounts.selection = ref
    }

    /// The system asks for a row's item as it lifts; the row shows as lifted until it lets go.
    private func lift(_ id: String) -> NSItemProvider {
        lifted = id
        return LiftItem { Task { @MainActor in if lifted == id { lifted = nil } } }
    }

    // MARK: order

    /// Writes a dragged order to each computer as before: its own bots, in an order the host syncs to every device.
    /// `unpinned` is nil when only the pinned rooms moved, so Automatic never overwrites the stored manual order.
    private func writeOrder(pinned: [Room], unpinned: [Room]?) {
        for store in shownStores {
            let id = store.computer.id
            let base = store.roster
            let basePinned = base.filter(\.pinned).map(\.id)
            let newPinned = pinned.filter { $0.item.ref.computerId == id }.map { $0.item.bot.id }
            let rest = unpinned?.filter { $0.item.ref.computerId == id }.map { $0.item.bot.id } ?? base.filter { !$0.pinned }.map(\.id)
            let ids = (newPinned.isEmpty ? basePinned : newPinned) + rest
            if ids != base.map(\.id) { store.reorder(ids) }
        }
    }

    private func movePinned(_ pinned: [Room]) -> ((IndexSet, Int) -> Void)? {
        { source, destination in
            var after = pinned
            after.move(fromOffsets: source, toOffset: destination)
            writeOrder(pinned: after, unpinned: nil)
        }
    }

    private func moveRest(pinned: [Room], rest: [Room]) -> (IndexSet, Int) -> Void {
        { source, destination in
            var after = rest
            after.move(fromOffsets: source, toOffset: destination)
            writeOrder(pinned: pinned, unpinned: after)
        }
    }

    /// VoiceOver's Move up / Move down.
    private func shift(_ room: Room, by offset: Int, in section: [Room]) {
        guard let i = section.firstIndex(where: { $0.id == room.id }), section.indices.contains(i + offset) else { return }
        var after = section
        after.swapAt(i, i + offset)
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) {
            if room.item.bot.pinned {
                writeOrder(pinned: after, unpinned: nil)
            } else {
                writeOrder(pinned: rooms.filter { $0.item.bot.pinned }, unpinned: after)
            }
        }
    }

    // MARK: top of the list

    @ViewBuilder private func updateCard(_ store: BotStore) -> some View {
        if let mismatch = store.mismatch {
            UpdateNeededCard(store: store, mismatch: mismatch)
                .padding(.horizontal, KithSpacing.gutter)
                .padding(.vertical, 8)
                .transition(.opacity)
        }
    }

    /// Machines show only when something is wrong with one: a chip that opens Computers.
    @ViewBuilder private var machineChips: some View {
        let trouble = shownStores.filter { $0.mismatch == nil && ($0.isOffline || $0.shownConnection == .connecting) }
        if !trouble.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(trouble, id: \.computer.id) { store in
                        Button { app.showComputers = true } label: {
                            HStack(spacing: 6) {
                                Text(store.hostName).kithText(.secondary).foregroundStyle(Palette.ink).lineLimit(1)
                                StatusChip(store.isOffline ? .macUnreachable : .catchingUp)
                            }
                            .padding(.horizontal, 12).padding(.vertical, 6)
                            .kithGlass(in: Capsule(), interactive: true)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("\(store.hostName), \(store.connectionLabel). Opens Computers")
                    }
                }
                .padding(.horizontal, KithSpacing.gutter)
            }
            .padding(.vertical, 4)
        }
    }

    /// Guests' requests waiting for this account's answer, on the shown computers it owns, ahead of the rooms.
    @ViewBuilder private var needsYourOK: some View {
        let waiting = shownStores.flatMap { store in
            accounts.guestPrompts(for: store.computer.id).map { (store, $0) }
        }
        let requests = shownStores.flatMap { store in
            accounts.botRequests(for: store.computer.id).map { (store, $0) }
        }
        if !waiting.isEmpty || !requests.isEmpty {
            Text("Needs your OK")
                .kithText(.secondary).fontWeight(.semibold)
                .foregroundStyle(Palette.muted)
                .padding(.horizontal, KithSpacing.gutter)
                .padding(.top, 10)
                .padding(.bottom, 4)
                .accessibilityAddTraits(.isHeader)
            // Bots from outside first, then the guests' permission cards.
            ForEach(requests, id: \.1.id) { store, bot in
                VStack(alignment: .leading, spacing: 4) {
                    if accounts.computers.count > 1 {
                        Text(store.hostName).kithText(.secondary).foregroundStyle(Palette.muted)
                    }
                    DecisionCard(DecisionCardModel(botRequest: bot), answering: store.botGuests.deciding[bot.botId]) { option in
                        Task { await store.botGuests.decide(bot, option: option) }
                    }
                }
                .padding(.horizontal, KithSpacing.gutter)
                .padding(.vertical, 4)
                .transition(.opacity)
            }
            ForEach(waiting, id: \.1.id) { store, prompt in
                VStack(alignment: .leading, spacing: 4) {
                    if accounts.computers.count > 1 {
                        Text(store.hostName).kithText(.secondary).foregroundStyle(Palette.muted)
                    }
                    DecisionCard(DecisionCardModel(guest: prompt), answering: store.answering[prompt.entry.id]) {
                        store.respond(prompt.entry, option: $0)
                    }
                }
                .padding(.horizontal, KithSpacing.gutter)
                .padding(.vertical, 4)
                .transition(.opacity)
            }
        }
    }

    // MARK: dock

    /// Typing leads somewhere when it matches a room and nothing is attached.
    private func leads(_ hasMatch: Bool) -> Bool { searching && files.isEmpty && hasMatch }

    /// The same dock on both pages; `submit` is what Return and the send arrow do on this one.
    private func dock(leading: Bool, submit: @escaping () -> Void) -> some View {
        ChatDock(text: dockText(submit), placeholder: "Find or ask…",
                 hasAttachment: !files.isEmpty,
                 onSend: submit,
                 onMic: { dictation.toggle(current: query) { query = $0 } },
                 onCall: { call() },
                 onPhotos: { pickingPhotos = true },
                 onCamera: { if CameraPicker.isAvailable { pickingCamera = true } else { accounts.lastError = "This device has no camera." } },
                 onFiles: { pickingFiles = true },
                 sendStyle: leading ? .muted : .word, micListening: dictation.listening) {
            if !files.isEmpty {
                HomeAttachmentChips(files: files) { file in
                    withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { files.removeAll { $0.id == file.id } }
                }
            }
        }
    }

    /// Return at the end of the text submits it; the box is otherwise a plain field.
    private func dockText(_ submit: @escaping () -> Void) -> Binding<String> {
        Binding(get: { query }, set: { new in
            if new.hasSuffix("\n"), String(new.dropLast()) == query { submit() } else { query = new }
        })
    }

    /// Return and the send arrow: open the top match, or, with nothing to open, pick an agent for a new session.
    private func submit(_ matches: [Room]) {
        dictation.stop()
        if leads(!matches.isEmpty), let top = matches.first {
            open(top.item.ref)
        } else if searching || !files.isEmpty {
            picking = .send
        }
    }

    /// The same on the Board: the top matching item's chat, or the pick-an-agent sheet.
    private func submitBoard(_ board: BoardModel) {
        dictation.stop()
        if leads(board.top != nil), let top = board.top {
            openBoard(top)
        } else if searching || !files.isEmpty {
            picking = .send
        }
    }

    private func openBoard(_ target: BoardTarget) {
        open(BotReference(accountId: accounts.accountId, computerId: target.computerId, botId: target.botId))
    }

    /// One agent is called straight away; with several, the person picks.
    private func call() {
        dictation.stop()
        guard calling == nil, picking == nil else { return }
        let agents = rooms.filter { !$0.item.bot.isGroup }
        if agents.count == 1, let only = agents.first { startCall(only.store, only.item.bot.id) } else { picking = .call }
    }

    private func startCall(_ store: BotStore, _ botId: String) {
        dictation.stop()
        if let block = HomeCallCover.block(store: store, botId: botId) {
            store.lastError = block
        } else {
            calling = CallTarget(store: store, botId: botId)
        }
    }

    /// The agents to pick from, for a new session or a call.
    private func pickChoices(for pick: AgentPick) -> [PickAgentSheet.Choice] {
        displayedAgents.map { room in
            PickAgentSheet.Choice(model: CrewRows.model(for: room.item, store: room.store, team: room.team)) {
                switch pick {
                case .call: startCall(room.store, room.item.bot.id)
                case .send: startSession(with: room)
                }
            }
        }
    }

    private var displayedAgents: [Room] { displayed(rooms).filter { !$0.item.bot.isGroup } }

    /// A new session with the agent: what was typed and attached goes out as its first message, then its chat opens.
    private func startSession(with room: Room) {
        let text = trimmedQuery
        room.store.send(text, to: room.item.bot.id, files: files, newTask: true)
        query = ""
        files = []
        accounts.selection = room.item.ref
    }

    private func append(_ new: [OutgoingFile]) {
        let result = HomeAttachments.accepted(new)
        if let error = result.error { accounts.lastError = error }
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { files += result.files }
    }

    private func addPhotos(_ items: [PhotosPickerItem]) {
        Task {
            var images: [Data] = []
            for item in items { if let data = try? await item.loadTransferable(type: Data.self) { images.append(data) } }
            append(HomeAttachments.photos(images))
        }
    }

    private func addFiles(_ urls: [URL]) { append(HomeAttachments.files(from: urls)) }

    // MARK: new things

    /// A group chat gathers bots of one computer (the first one online; more computers are future work).
    private func newGroup() {
        guard let store = onlineStores.first else { return }
        Task {
            // Let the menu fold away before the editor slides up.
            try? await Task.sleep(for: .milliseconds(200))
            editingGroup = GroupTarget(computerId: store.computer.id, group: nil)
        }
    }

    /// A new bot starts on the first computer online; the editor's Computer row can move it.
    private func newBot() {
        guard let store = onlineStores.first ?? allStores.first(where: { $0.mismatch == nil }) else { return }
        editing = EditTarget(computerId: store.computer.id, draft: BotDraft())
    }
}

/// Everything Crew presents: the editors, dialogs and the dock's pickers.
private struct HomeSheets: ViewModifier {
    @Environment(AccountStore.self) private var accounts
    @Binding var editing: EditTarget?
    @Binding var editingGroup: GroupTarget?
    @Binding var confirmDelete: RosterItem?
    @Binding var newChat: BotListView.ChatTarget?
    @Binding var inviting: BotListView.ChatTarget?
    @Binding var botSheet: BotSheetTarget?
    @Binding var sharingAgent: BotListView.ShareAgentTarget?
    let allStores: [BotStore]
    @Binding var query: String
    @Binding var files: [OutgoingFile]
    @Binding var picking: AgentPick?
    @Binding var calling: BotListView.CallTarget?
    @Binding var pickingPhotos: Bool
    @Binding var photoItems: [PhotosPickerItem]
    @Binding var pickingFiles: Bool
    @Binding var pickingCamera: Bool
    let pickChoices: (AgentPick) -> [PickAgentSheet.Choice]
    let addPhotos: ([PhotosPickerItem]) -> Void
    let addFiles: ([URL]) -> Void
    let addCamera: (Data) -> Void

    func body(content: Content) -> some View {
        content
            .kithSheet(item: $editing) { target in
                // A new bot can move to another computer until it's created.
                let computerId = editing?.computerId ?? target.computerId
                if let store = accounts.store(for: computerId) {
                    BotEditorView(draft: target.draft,
                                  computers: allStores.map {
                                      ($0.computer.id, $0.connection == .online ? $0.hostName : "\($0.hostName) (offline)")
                                  },
                                  computer: target.draft.id == nil
                                      ? Binding(get: { computerId }, set: { editing?.computerId = $0 })
                                      : nil)
                        .environment(store)
                        .onChange(of: store.selection) { _, botId in
                            // A new bot opens its chat, on the computer it was created on.
                            guard let botId else { return }
                            store.selection = nil
                            accounts.selection = BotReference(accountId: accounts.accountId, computerId: computerId, botId: botId)
                        }
                }
            }
            .kithSheet(item: $newChat) { target in
                NewChatSheet(store: target.store).environment(accounts)
            }
            .kithSheet(item: $inviting) { target in
                InvitePeopleSheet(store: target.store)
            }
            .botSheets($botSheet, viewer: .owner)
            .kithSheet(item: $sharingAgent) { target in
                ShareAgentSheet(store: target.store, agent: target.agent, name: target.name)
            }
            .kithSheet(item: $editingGroup) { target in
                if let store = accounts.store(for: target.computerId) {
                    GroupEditorView(group: target.group)
                        .environment(store)
                        .onChange(of: store.selection) { _, botId in
                            guard let botId else { return }
                            store.selection = nil
                            accounts.selection = BotReference(accountId: accounts.accountId, computerId: target.computerId, botId: botId)
                        }
                }
            }
            .kithSheet(item: $picking) { pick in
                PickAgentSheet(title: pick.title, choices: pickChoices(pick))
            }
            .fullScreenCover(item: $calling) { target in
                HomeCallCover(store: target.store, botId: target.botId) { calling = nil }
            }
            .kithDialog("Delete \(confirmDelete?.bot.name ?? "agent")?",
                          isPresented: Binding(get: { confirmDelete != nil }, set: { if !$0 { confirmDelete = nil } }),
                          message: confirmDelete?.bot.isGroup == true ? "Its bots and their own chats stay." : "Files it changed on your computer stay as they are.") {
                let item = confirmDelete
                return [DialogAction(item?.bot.isGroup == true ? "Delete group chat" : "Delete agent and its conversation", destructive: true) {
                    if let item { accounts.store(for: item.ref.computerId)?.delete(item.bot) }
                }]
            }
            .photosPicker(isPresented: $pickingPhotos, selection: $photoItems, maxSelectionCount: 10, matching: .images)
            .onChange(of: photoItems) { _, items in
                guard !items.isEmpty else { return }
                photoItems = []
                addPhotos(items)
            }
            .fileImporter(isPresented: $pickingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
                if case .success(let urls) = result { addFiles(urls) }
            }
            .fullScreenCover(isPresented: $pickingCamera) {
                CameraPicker(picked: addCamera).ignoresSafeArea()
            }
    }
}

/// People on a computer, from the new-chat menu and a row's Share: who is invited, and the way to invite someone.
private struct InvitePeopleSheet: View {
    let store: BotStore
    @State private var people: PeopleStore

    init(store: BotStore) {
        self.store = store
        _people = State(initialValue: PeopleStore(store: store))
    }

    var body: some View {
        PeopleView(people: people, store: store)
            .task { await people.load() }
    }
}

/// Makes a row draggable (and shows it lifted) only where the order setting allows.
private struct LiftIfReorderable: ViewModifier {
    let reorderable: Bool
    let provider: () -> NSItemProvider

    func body(content: Content) -> some View {
        if reorderable { content.itemProvider(provider) } else { content }
    }
}

/// The item a lifted room row carries (nothing): the system lets go of it when the drag or the
/// menu ends.
private final class LiftItem: NSItemProvider {
    private let ended: @Sendable () -> Void

    init(ended: @escaping @Sendable () -> Void) {
        self.ended = ended
        super.init()
    }

    deinit { ended() }
}

private struct GroupTarget: Identifiable {
    let id = UUID()
    let computerId: ComputerID
    let group: Bot?
}

private struct EditTarget: Identifiable {
    let id = UUID()
    var computerId: ComputerID
    let draft: BotDraft
}

/// The crew before there is one: on the sky, in the new type.
private struct EmptyRoster: View {
    let canCreate: Bool
    let hasComputer: Bool
    let create: () -> Void
    let showComputers: () -> Void

    var body: some View {
        VStack(spacing: 14) {
            CharacterAvatar(portrait: "spare-cloud", size: 72)
            Text("No agents yet").kithText(.agentName).foregroundStyle(Palette.ink)
            Text(hasComputer
                 ? "Make an agent for each kind of work: a reviewer, a fixer, a docs writer. Point it at a project."
                 : "Ask one of your account's computers for access, or pair one with its code.")
                .kithText(.body)
                .foregroundStyle(Palette.muted)
                .multilineTextAlignment(.center)
            if canCreate {
                Button("Make your first agent", action: create)
                    .buttonStyle(.primary)
            } else if !hasComputer {
                Button("Computers", action: showComputers)
                    .buttonStyle(.primary)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 32)
        .padding(.vertical, 40)
    }
}

/// The width of home's pages, to slide one out while the other slides in.
private struct PageWidthKey: PreferenceKey {
    static let defaultValue: CGFloat = 390
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = nextValue() }
}
