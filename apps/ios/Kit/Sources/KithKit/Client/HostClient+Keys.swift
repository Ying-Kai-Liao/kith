import Foundation

/// Keys (serve/host/src/cards/secret.rs): the owner's, over the E2E channel from a device with Control scope. The key
/// goes to the computer's Keychain and nowhere else: this file never logs, stores or returns a value.
public extension HostClient {
    /// Saves the key an Access or Key card asked for. `account` is the label for a new key (nil on a replace); `values`
    /// is one string per variable of the card. The host answers with names only. Errors carry the host's text
    /// ("That key is too long", "Couldn't save it to the Keychain").
    func provideSecret(entryId: String, account: String?, values: [String: String]) async throws -> SecretSaved {
        struct Body: Encodable { var entryId: String; var account: String?; var values: [String: String] }
        struct Res: Decodable { var saved: SecretSaved }
        let res: Res = try await call("provideSecret", Body(entryId: entryId, account: account, values: values), timeout: 60)
        return res.saved
    }

    /// Every key on the computer: names, agents that hold each and when it was last used.
    func listKeys() async throws -> [KeyRecord] {
        struct Res: Decodable { var keys: [KeyRecord] }
        let res: Res = try await call("listKeys")
        return res.keys
    }

    /// Takes one agent off a key. The key stays in the Keychain for the others.
    func revokeKey(agent: String, service: String, profile: String) async throws {
        struct Body: Encodable { var agent: String; var service: String; var profile: String }
        let _: Empty = try await call("revokeKey", Body(agent: agent, service: service, profile: profile), timeout: 30)
    }

    /// What "Always allow" remembered for an agent in auto mode (serve/host/src/cards/autoallow.rs). Owner only.
    func allowedRules(agent: String) async throws -> AllowedList {
        struct Body: Encodable { var agent: String }
        return try await call("allowedRules", Body(agent: agent))
    }

    /// Forgets one rule; the agent's next run no longer has it. A rule already gone answers "Not found".
    func removeAllowedRule(agent: String, id: String) async throws {
        struct Body: Encodable { var agent: String; var id: String }
        let _: Empty = try await call("removeAllowedRule", Body(agent: agent, id: id), timeout: 30)
    }
}
