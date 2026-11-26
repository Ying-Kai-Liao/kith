import KithKit
import KithUI
import SwiftUI

/// The Feed place's screen. Until the feed is built it shows today's Board, wired as it was when it was home's second
/// page, so nothing people rely on goes away. The Feed package replaces this view and nothing around it.
struct FeedPlaceholderView: View {
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts
    /// Computers left out of the list on this device, as in Crew.
    @AppStorage("hiddenComputers") private var hiddenComputers = ""

    private var shownStores: [BotStore] {
        let shown = Set(ComputerSelection(all: accounts.computers.map(\.id), hidden: hiddenComputers).shown)
        return accounts.computers.filter { shown.contains($0.id) }.compactMap { accounts.store(for: $0.id) }
    }

    private var model: BoardModel {
        BoardModel(computers: shownStores.map {
            BoardComputer(store: $0, guestPrompts: accounts.guestPrompts(for: $0.computer.id), botRequests: accounts.botRequests(for: $0.computer.id))
        })
    }

    var body: some View {
        BoardView(model: model, searching: false, showsHosts: accounts.computers.count > 1,
                  store: { accounts.store(for: $0) }, handoffStores: shownStores.filter(\.isOwner),
                  open: { target in
                      app.openBot(BotReference(accountId: accounts.accountId, computerId: target.computerId, botId: target.botId))
                  },
                  showComputers: { app.showComputers = true })
    }
}
