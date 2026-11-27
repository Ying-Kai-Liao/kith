import KithKit
import KithUI
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

/// The Feed place: its own navigation stack under the shared sky and top bar. The feed screen is `FeedPlaceholderView`,
/// the one view the Feed package replaces.
struct HomeFeedStack: View {
    @Binding var tab: HomeTab

    var body: some View {
        NavigationStack {
            ZStack(alignment: .top) {
                SkyBackground(.home)
                FeedPlaceholderView()
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                // The same width as Crew's new-chat button, so the switch sits where it does there.
                HomeTopBar(tab: $tab) { Color.clear.frame(width: 44, height: 44) }
            }
            .toolbar(.hidden, for: .navigationBar)
        }
        .tint(Palette.text)
    }
}
