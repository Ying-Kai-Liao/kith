import Foundation

/// "In 10 min" on an incoming call: a local notification that rings the same agent again.
/// One pending notification per agent, so pressing it twice, or a second call from the same agent, replaces it.
public struct CallSnooze: Equatable, Sendable {
    public static let delay: TimeInterval = 10 * 60
    public static let category = "snoozedCall"

    public var ctx: String
    public var computerId: String
    public var botId: String
    public var name: String

    public init(ctx: String, computerId: String, botId: String, name: String) {
        self.ctx = ctx
        self.computerId = computerId
        self.botId = botId
        self.name = name
    }

    /// Stable, so scheduling again replaces the pending one.
    public var identifier: String { "snoozed-call/\(ctx)/\(computerId)/\(botId)" }

    public var title: String { "\(name) is calling" }
    public var body: String { "You asked to be called back. Tap to pick up." }

    /// Read back by the app's notification handler; `callBack` marks a tap that opens the call, not the chat.
    public var userInfo: [String: String] {
        ["ctx": ctx, "computerId": computerId, "botId": botId, "callBack": "1"]
    }

    public static func isCallBack(_ userInfo: [AnyHashable: Any]) -> Bool { userInfo["callBack"] as? String == "1" }
}
