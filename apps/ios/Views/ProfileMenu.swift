import KithKit
import KithUI
import SwiftUI

/// The picture at the top left of Crew: your sky patch (your photo once you have one) in a ring. A stock menu with
/// the account (its subtitle is the Macs' health line), Computers (opens Account > Your devices),
/// Extensions, Usage and Settings.
struct ProfileMenu: View {
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts
    @Environment(AccountSession.self) private var account
    @AppStorage("hiddenComputers") private var hiddenComputers = ""

    /// A guest (joined computers by invite only) has no usage, extensions or widgets: those are the owner's.
    private var ownerThings: Bool { accounts.computers.isEmpty || !accounts.ownedComputers.isEmpty }

    private var accountTitle: String { account.email ?? "This iPhone" }

    private var accountSubtitle: String {
        if account.isSignedIn { return ComputerProblem.healthLine(in: accounts, hidden: hiddenComputers) }
        let count = accounts.computers.count
        return count == 0 ? "Local pairing" : "Local pairing · \(count) Mac\(count == 1 ? "" : "s")"
    }

    var body: some View {
        Menu {
            Button { account.showSwitcher = true } label: {
                Text(accountTitle)
                Text(accountSubtitle)
                Image(systemName: "person.crop.circle")
            }
            Divider()
            Button { app.showComputers = true } label: {
                Label("Computers", systemImage: "desktopcomputer")
            }
            if ownerThings {
                Button { app.marketplace = app.currentStore?.computer.id } label: {
                    Label("Extensions", systemImage: "puzzlepiece.extension")
                }
                .disabled(app.currentStore == nil)
                Button { app.showUsage = true } label: {
                    Label("Usage", systemImage: "chart.bar")
                }
            }
            Button { app.showSettings = true } label: {
                Label("Settings", systemImage: "gearshape")
            }
        } label: {
            picture
                .frame(width: KithSize.topButton, height: KithSize.topButton)
                .clipShape(Circle())
                .overlay(Circle().strokeBorder(.white.opacity(0.7), lineWidth: 1.5))
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        }
        .accessibilityLabel(Text("Account and settings"))
        .accessibilityValue(Text(accountTitle))
    }

    @ViewBuilder private var picture: some View {
        if let url = account.avatarURL {
            AccountAvatar(url: url, email: account.email, size: KithSize.topButton)
        } else {
            SkyPatch(id: account.userID ?? "you", size: KithSize.topButton, isYou: true)
        }
    }
}
