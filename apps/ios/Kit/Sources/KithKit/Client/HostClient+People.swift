import Foundation

/// The one call an invited person's device makes first.
public protocol InviteHost: Sendable {
    func proveInvite(inviteId: String, token: String) async throws
}

/// Inviting people to a computer (serve/host/src/api/people.rs). The owner-only methods answer an invited
/// person's device with a refusal, and an older host with "unknown method" (`HostError.http(404, …)`).
public extension HostClient {
    func people() async throws -> [HostPerson] {
        struct Res: Decodable { var people: [HostPerson] }
        let res: Res = try await call("people")
        return res.people
    }

    func invites() async throws -> [HostInvite] {
        struct Res: Decodable { var invites: [HostInvite] }
        let res: Res = try await call("invites")
        return res.invites
    }

    /// `name` for someone new, or `personId` for someone already invited; `agents` maps an agent to a role.
    func createInvite(name: String?, personId: String?, agents: [String: InviteRole]) async throws -> CreatedInvite {
        struct Body: Encodable { var name: String?; var personId: String?; var agents: [String: InviteRole] }
        // Writing to kith-auth and the cloud: slower than a read.
        return try await call("createInvite", Body(name: name, personId: personId, agents: agents), timeout: 60)
    }

    /// Members' devices waiting for the owner's OK (an older computer answers this phone with a refusal).
    func accessRequests() async throws -> [MemberAccessRequest] {
        struct Res: Decodable { var requests: [MemberAccessRequest] }
        let res: Res = try await call("accessRequests")
        return res.requests
    }

    func decideAccessRequest(_ requestId: String, approve: Bool) async throws {
        struct Body: Encodable { var requestId: String; var approve: Bool }
        // The computer tells the cloud before it answers.
        let _: Empty = try await call("decideAccessRequest", Body(requestId: requestId, approve: approve), timeout: 30)
    }

    func revokeInvite(_ inviteId: String) async throws {
        let _: Empty = try await call("revokeInvite", ["inviteId": inviteId], timeout: 30)
    }

    /// Replaces what the person may use; a nil list is left as it is.
    func setPersonAccess(personId: String, agents: [String: InviteRole]?, projects: [String]?) async throws {
        struct Body: Encodable { var personId: String; var agents: [String: InviteRole]?; var projects: [String]? }
        let _: Empty = try await call("setPersonAccess", Body(personId: personId, agents: agents, projects: projects), timeout: 60)
    }

    /// Ends the person's access. Their chats and memory stay on the computer.
    func revokePerson(_ personId: String) async throws {
        let _: Empty = try await call("revokePerson", ["personId": personId], timeout: 60)
    }

    /// Undoes `revokePerson`. The host says why when it can't (the person was removed meanwhile).
    func unrevokePerson(_ personId: String) async throws {
        let _: Empty = try await call("unrevokePerson", ["personId": personId], timeout: 60)
    }

    func audit(since: Date? = nil, personId: String? = nil) async throws -> [AuditRow] {
        struct Body: Encodable { var since: Int64?; var personId: String? }
        struct Res: Decodable { var audit: [AuditRow] }
        let res: Res = try await call("audit", Body(since: since.map { Int64($0.timeIntervalSince1970 * 1000) }, personId: personId))
        return res.audit
    }

    /// An invited person's own chat with a granted agent (the host makes it the first time).
    func startChat(agent: String) async throws -> Bot {
        struct Res: Decodable { var bot: Bot }
        let res: Res = try await call(.startChat, ["agent": agent], timeout: 60)
        return res.bot
    }

    /// Shows the invite token from the link to the computer. Until it succeeds, the computer refuses everything else.
    func proveInvite(inviteId: String, token: String) async throws {
        let _: Empty = try await call(.proveInvite, ["inviteId": inviteId, "token": token])
    }
}

extension HostClient: InviteHost {}

public enum InviteProof {
    /// The computer says this device already showed its invite: a success when repeated.
    public static func isAlreadyProven(_ error: Error) -> Bool {
        error.localizedDescription.localizedCaseInsensitiveContains("already shown its invite")
    }

    /// An older host, or one that doesn't take invites: the call doesn't exist there.
    public static func isUnknownMethod(_ error: Error) -> Bool {
        guard case let HostError.http(status, message) = error else { return false }
        return status == 404 || message.localizedCaseInsensitiveContains("unknown method")
    }
}
