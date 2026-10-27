import Foundation

/// Bot guests (serve/host/src/api/botguests.rs): bots from outside that ask an owner's agents. `bots`, `decideBot` and
/// `setAutoAllowBots` are the computer's owner's; an invited person makes and removes their own with the others. An
/// older host answers "unknown method" (`InviteProof.isUnknownMethod`), which hides the whole feature.
public extension HostClient {
    /// Every bot on the computer, with its owner's name, person id and conversations. The computer's owner only.
    func bots() async throws -> [BotGuest] {
        struct Res: Decodable { var bots: [BotGuest] }
        let res: Res = try await call("bots")
        return res.bots
    }

    /// The caller's own bots, ended ones too.
    func myBots() async throws -> [BotGuest] {
        struct Res: Decodable { var bots: [BotGuest] }
        let res: Res = try await call(.myBots)
        return res.bots
    }

    /// A new bot and its invite, or with `botId` a fresh invite for a bot already there. `provider` is "grok" or
    /// "other". The host's error text is for the person as written.
    func createBotInvite(name: String?, provider: String?, agents: [String], botId: String? = nil) async throws -> CreatedBotInvite {
        struct Body: Encodable { var name: String?; var provider: String?; var agents: [String]; var botId: String? }
        // Writes kith-auth and the cloud: slower than a read, and not safe to repeat.
        return try await call(.createBotInvite, Body(name: name, provider: provider, agents: agents, botId: botId), timeout: 60)
    }

    /// "allow" or "deny" a waiting bot; answering twice is fine. `alwaysForOwner` also lets that person's bots join on
    /// their own from now on.
    @discardableResult
    func decideBot(botId: String, decision: BotDecision, alwaysForOwner: Bool = false) async throws -> BotGuest? {
        struct Body: Encodable { var botId: String; var decision: String; var alwaysForOwner: Bool? }
        struct Res: Decodable { var bot: BotGuest? }
        let res: Res = try await call("decideBot", Body(botId: botId, decision: decision.rawValue,
                                                        alwaysForOwner: alwaysForOwner ? true : nil), timeout: 30)
        return res.bot
    }

    /// Turns "always allow this person's bots" on or off; on lets their waiting bots in.
    func setAutoAllowBots(personId: String, on: Bool) async throws {
        struct Body: Encodable { var personId: String; var on: Bool }
        let _: Empty = try await call("setAutoAllowBots", Body(personId: personId, on: on), timeout: 30)
    }

    /// Ends the bot: it can't ask any more and what it is running stops. Removing twice is fine.
    func revokeBot(botId: String) async throws {
        let _: Empty = try await call(.revokeBot, ["botId": botId], timeout: 60)
    }
}

public enum BotDecision: String, Sendable {
    case allow, deny
}
