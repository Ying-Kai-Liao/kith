import KithKit
import KithUI
import PhotosUI
import SwiftUI

/// The top bar of both places, as the mock's tabTop(): your picture (the profile menu) on the left, the Crew | Feed
/// switch in the middle, and whatever the place needs on the right.
struct HomeTopBar<Trailing: View>: View {
    @Binding var tab: HomeTab
    @ViewBuilder let trailing: Trailing

    var body: some View {
        HStack {
            ProfileMenu()
            Spacer(minLength: 8)
            HomeTabSwitch(selection: $tab)
            Spacer(minLength: 8)
            trailing
        }
        .padding(.horizontal, KithSpacing.gutter)
        .padding(.top, 4)
    }
}

/// The Feed place: its own navigation stack under the shared sky and top bar, showing the owner's Feed.
struct HomeFeedStack: View {
    @Binding var tab: HomeTab
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    // The box's + and call, staged the way Crew's dock stages them.
    @State private var files: [OutgoingFile] = []
    @State private var pickingPhotos = false
    @State private var photoItems: [PhotosPickerItem] = []
    @State private var pickingFiles = false
    @State private var pickingCamera = false
    @State private var picking: AgentPick?
    @State private var calling: BotListView.CallTarget?

    /// The feed is the owner's: the first computer you own.
    private var feedStore: BotStore? {
        accounts.computers.lazy.compactMap { accounts.store(for: $0.id) }.first { $0.isOwner }
    }

    private static var isDemo: Bool {
        #if DEBUG
        DemoScreens.name == "feed"
        #else
        false
        #endif
    }

    /// Opens a chat the way Crew does: through `AppStore.openBot`, landing on the entry when given.
    private func openChat(_ store: BotStore, chat: String, entry: String?) {
        store.focusEntry = entry.map { .init(chat: chat, entry: $0) }
        app.openBot(BotReference(accountId: accounts.accountId, computerId: store.computer.id, botId: chat))
    }

    var body: some View {
        NavigationStack {
            ZStack(alignment: .top) {
                SkyBackground(.home)
                if Self.isDemo {
                    #if DEBUG
                    FeedDemoScreen()
                    #endif
                } else if let store = feedStore {
                    FeedView(store: store, attach: attach(store)) { chat, entry in openChat(store, chat: chat, entry: entry) }
                        .id(store.computer.id)
                } else {
                    Text("Connect a computer to see the Feed.").foregroundStyle(Palette.secondary)
                }
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                // The same width as Crew's new-chat button, so the switch sits where it does there.
                HomeTopBar(tab: $tab) { Color.clear.frame(width: 44, height: 44) }
            }
            .toolbar(.hidden, for: .navigationBar)
        }
        .tint(Palette.text)
        .modifier(feedSheets)
    }

    // MARK: the box's + and call, as Crew's

    private func attach(_ store: BotStore) -> FeedAttach {
        FeedAttach(
            hasFiles: !files.isEmpty,
            strip: AnyView(HomeAttachmentChips(files: files) { file in
                withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { files.removeAll { $0.id == file.id } }
            }),
            onCall: { call(store) },
            onPhotos: { pickingPhotos = true },
            onCamera: { if CameraPicker.isAvailable { pickingCamera = true } else { accounts.lastError = "This device has no camera." } },
            onFiles: { pickingFiles = true },
            onSent: { chat in
                // The text went to this chat; the staged files follow it there.
                guard !files.isEmpty else { return }
                store.send("", to: chat, files: files)
                files = []
            })
    }

    private func agents(_ store: BotStore) -> [RosterItem] {
        accounts.roster.filter { $0.ref.computerId == store.computer.id && !$0.bot.isGroup }
    }

    /// One agent is called straight away; with several, the person picks.
    private func call(_ store: BotStore) {
        guard calling == nil, picking == nil else { return }
        let all = agents(store)
        if all.count == 1, let only = all.first { startCall(store, only.bot.id) } else { picking = .call }
    }

    private func startCall(_ store: BotStore, _ botId: String) {
        if let block = HomeCallCover.block(store: store, botId: botId) {
            store.lastError = block
        } else {
            calling = BotListView.CallTarget(store: store, botId: botId)
        }
    }

    private var feedSheets: FeedSheets {
        FeedSheets(
            picking: $picking, calling: $calling, pickingPhotos: $pickingPhotos, photoItems: $photoItems,
            pickingFiles: $pickingFiles, pickingCamera: $pickingCamera,
            choices: {
                guard let store = feedStore else { return [] }
                return agents(store).map { item in
                    PickAgentSheet.Choice(model: CrewRows.model(for: item, store: store, team: false)) { startCall(store, item.bot.id) }
                }
            },
            addPhotos: { items in
                Task {
                    var images: [Data] = []
                    for item in items { if let data = try? await item.loadTransferable(type: Data.self) { images.append(data) } }
                    append(HomeAttachments.photos(images))
                }
            },
            addFiles: { append(HomeAttachments.files(from: $0)) },
            addCamera: { append(HomeAttachments.photos([$0])) })
    }

    private func append(_ new: [OutgoingFile]) {
        let result = HomeAttachments.accepted(new)
        if let error = result.error { accounts.lastError = error }
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { files += result.files }
    }
}

/// The Feed's pickers and call cover, the same ones Crew's dock raises.
private struct FeedSheets: ViewModifier {
    @Binding var picking: AgentPick?
    @Binding var calling: BotListView.CallTarget?
    @Binding var pickingPhotos: Bool
    @Binding var photoItems: [PhotosPickerItem]
    @Binding var pickingFiles: Bool
    @Binding var pickingCamera: Bool
    let choices: () -> [PickAgentSheet.Choice]
    let addPhotos: ([PhotosPickerItem]) -> Void
    let addFiles: ([URL]) -> Void
    let addCamera: (Data) -> Void

    func body(content: Content) -> some View {
        content
            .kithSheet(item: $picking) { pick in PickAgentSheet(title: pick.title, choices: choices()) }
            .fullScreenCover(item: $calling) { target in
                HomeCallCover(store: target.store, botId: target.botId) { calling = nil }
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
            .fullScreenCover(isPresented: $pickingCamera) { CameraPicker(picked: addCamera).ignoresSafeArea() }
    }
}

#if DEBUG
/// `-kithDemoScreens feed`: the Feed over invented posts.
private struct FeedDemoScreen: View {
    @State private var parts = FeedDemo.make()
    var body: some View { FeedView(feed: parts.feed, composer: parts.composer) { _, _ in } }
}
#endif
