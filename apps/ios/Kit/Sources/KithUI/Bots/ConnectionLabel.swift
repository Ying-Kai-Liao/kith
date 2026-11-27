import KithKit
import SwiftUI

public extension BotStore {
    var connectionLabel: String {
        switch shownConnection {
        case .online where mismatch != nil: "Needs update"
        case .online: "Connected"
        case .connecting: "Connecting…"
        case .computerOffline, .offline: "Offline"
        case .unauthorized: "No access"
        case .unpaired: "Not paired"
        }
    }
}
