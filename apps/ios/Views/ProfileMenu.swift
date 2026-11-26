import KithKit
import KithUI
import SwiftUI

/// The picture at the top left of Crew: your sky patch (your photo once you have one) in a ring. A tap opens You: the
/// account, Usage, Routines, Computers and the settings pages.
struct ProfileMenu: View {
    @Environment(AccountSession.self) private var account
    @State private var showYou = false

    private var accountTitle: String { account.email ?? "This iPhone" }

    var body: some View {
        Button { showYou = true } label: {
            ProfilePicture()
                .frame(width: KithSize.topButton, height: KithSize.topButton)
                .clipShape(Circle())
                .overlay(Circle().strokeBorder(.white.opacity(0.7), lineWidth: 1.5))
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("You"))
        .accessibilityValue(Text(accountTitle))
        .kithSheet(isPresented: $showYou) {
            // Holds the pushes inside the sheet; no system bar shows.
            NavigationStack { YouView() }
        }
    }
}

/// Your sky patch, or your photo once you have one.
struct ProfilePicture: View {
    @Environment(AccountSession.self) private var account

    var body: some View {
        if let url = account.avatarURL {
            AccountAvatar(url: url, email: account.email, size: KithSize.topButton)
        } else {
            SkyPatch(id: account.userID ?? "you", size: KithSize.topButton, isYou: true)
        }
    }
}
