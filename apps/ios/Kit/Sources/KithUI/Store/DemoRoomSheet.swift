import KithKit
import SwiftUI

#if DEBUG

/// The sheets of the `room-add` and `room-info` demos, over the room the app has open.
public struct DemoRoomSheet: View {
    let store: BotStore
    let screen: String

    public init(store: BotStore, screen: String) {
        self.store = store
        self.screen = screen
    }

    public var body: some View {
        Group {
            if screen == "room-add", let group = store.bots[DemoScreens.roomId] {
                AddToRoomSheet(group: group, pick: "bg_grok")
            } else {
                RoomInfoView(groupId: DemoScreens.roomId)
            }
        }
        .environment(store)
        .tint(Palette.accent)
    }
}

#endif
