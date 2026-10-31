import Foundation
import Testing
@testable import KithKit

// Test tokens are obviously fake and low-entropy on purpose: realistic-looking secrets get flagged by scanners.
private let fakeToken = String(repeating: "a", count: 43)

// MARK: - the proof the cloud sees

@Test func proofIsLowercaseHexSha256OfTheUTF8Token() {
    // The same vector as desktop's invite.test.ts.
    #expect(InviteLink.proof(of: "test") == "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08")
    let h1 = InviteLink.proof(of: fakeToken)
    #expect(InviteLink.isProof(h1))
    #expect(h1 != fakeToken)
    #expect(!InviteLink.isProof(fakeToken))
    #expect(!InviteLink.isProof(h1.uppercased()))
    #expect(!InviteLink.isProof(String(h1.dropLast())))
}

// MARK: - parsing

private func link(_ query: String, scheme: String = "kith") -> URL { URL(string: "\(scheme)://invite?\(query)")! }

@Test func inviteURLParsesItsThreeParts() throws {
    let parsed = try #require(InviteLink(link("c=comp_1&i=inv_abc123&t=\(fakeToken)"), scheme: "kith"))
    #expect(parsed.computerId == "comp_1")
    #expect(parsed.inviteId == "inv_abc123")
    #expect(parsed.token == fakeToken)
    // The dev build has its own scheme.
    #expect(InviteLink(link("c=a&i=b&t=\(fakeToken)", scheme: "kith-dev"), scheme: "kith-dev") != nil)
}

@Test func inviteURLWithABadOrMissingPartIsRejected() {
    let t = fakeToken
    for query in [
        "i=inv_1&t=\(t)", "c=comp&t=\(t)", "c=comp&i=inv_1",
        "c=&i=inv_1&t=\(t)", "c=comp&i=&t=\(t)",
        "c=comp&i=inv_1&t=short",
        "c=comp/x&i=inv_1&t=\(t)", "c=comp&i=inv%201&t=\(t)", "c=comp&i=../x&t=\(t)", "c=comp&i=inv_1&t=\(t)%0A",
        "c=comp&i=\(String(repeating: "a", count: 129))&t=\(t)",
    ] {
        #expect(InviteLink(link(query), scheme: "kith") == nil, "\(query)")
    }
    // Another host or scheme is not an invite.
    #expect(InviteLink(URL(string: "kith://pair?c=comp&i=inv_1&t=\(t)")!, scheme: "kith") == nil)
    #expect(InviteLink(link("c=comp&i=inv_1&t=\(t)", scheme: "https"), scheme: "kith") == nil)
}

@Test func aPastedPageLinkGivesTheSamePartsTokenFromTheFragmentOnly() throws {
    for url in ["https://kithbot.app/i/comp_1/inv_abc#t=\(fakeToken)", "https://other.example/i/comp_1/inv_abc/#t=\(fakeToken)",
                "HTTPS://kithbot.app/i/comp_1/inv_abc#t=\(fakeToken)"] {
        let parsed = try #require(InviteLink(URL(string: url)!, scheme: "kith"), "\(url)")
        #expect(parsed.computerId == "comp_1")
        #expect(parsed.inviteId == "inv_abc")
        #expect(parsed.token == fakeToken)
    }
    for url in [
        "https://kithbot.app/i/comp_1/inv_abc",
        "https://kithbot.app/i/comp_1/inv_abc?t=\(fakeToken)",
        "https://kithbot.app/i/comp_1#t=\(fakeToken)",
        "https://kithbot.app/i/comp_1/#t=\(fakeToken)",
        "https://kithbot.app/x/comp_1/inv_abc#t=\(fakeToken)",
        "https://kithbot.app/i/comp_1/inv_abc/extra#t=\(fakeToken)",
        "http://kithbot.app/i/comp_1/inv_abc#t=\(fakeToken)",
        "https://kithbot.app/i/comp_1/inv_abc#t=short",
    ] {
        #expect(InviteLink(URL(string: url)!, scheme: "kith") == nil, "\(url)")
    }
    #expect(InviteLink.isPageLink(URL(string: "https://kithbot.app/i/comp_1/inv_abc")!))
    #expect(!InviteLink.isPageLink(URL(string: "https://example.com/other")!))
}

@Test func aLinkNeverPrintsItsToken() {
    let parsed = InviteLink(computerId: "comp", inviteId: "inv_1", token: fakeToken)
    #expect(!"\(parsed)".contains(fakeToken))
    #expect(!String(reflecting: parsed).contains(fakeToken))
    #expect(!"\(StoredInvite(inviteId: "inv_1", token: fakeToken))".contains(fakeToken))
}

// MARK: - the cloud accept call

@Test func acceptOnlyTakesAHexProofAndAValidInviteId() async {
    let cloud = CloudClient(baseURL: URL(string: "https://cloud.invalid")!, identity: nil) { "jwt" }
    for (id, proof) in [("inv_1", fakeToken), ("inv_1", "test"), ("inv_1", String(repeating: "A", count: 64)),
                        ("inv_1", String(repeating: "a", count: 63)), ("../x", InviteLink.proof(of: "test")), ("", InviteLink.proof(of: "test"))] {
        do {
            _ = try await cloud.acceptInvite(id, proof: proof)
            Issue.record("accepted \(id) / \(proof)")
        } catch let e as CloudError {
            // Refused here, before any request: the host name above doesn't resolve.
            #expect(e.code == "badInvite")
        } catch {
            Issue.record("sent a request: \(error)")
        }
    }
}

// MARK: - decoding

@Test func peopleDecodeFromTheHostShape() throws {
    let json = #"""
    {"people":[{"personId":"sam","name":"Sam","agents":{"house":"guest","work":"member","odd":"surprise"},
      "projects":["site"],"joined":true,"userId":"user_1","membershipId":"mem_1","openInvites":1,
      "devices":[{"key":"k1","name":"Sam's iPhone","platform":"ios","lastSeenAt":1700000000000,"connected":true},
                 {"key":"k2","name":null,"platform":null,"lastSeenAt":null,"connected":false}],
      "createdAt":1},
     {"personId":"lee","agents":{},"projects":[],"joined":false,"openInvites":0,"devices":[],"createdAt":2}]}
    """#
    struct Res: Decodable { var people: [HostPerson] }
    let people = try JSONDecoder().decode(Res.self, from: Data(json.utf8)).people
    #expect(people.count == 2)
    let sam = people[0]
    #expect(sam.name == "Sam")
    #expect(sam.agents == ["house": .guest, "work": .member, "odd": .guest])
    #expect(sam.projects == ["site"])
    #expect(sam.joined && sam.openInvites == 1)
    #expect(sam.devices.count == 2 && sam.devices[0].connected == true)
    #expect(sam.lastSeen == Date(timeIntervalSince1970: 1_700_000_000))
    // No name: the id stands in.
    #expect(people[1].name == "lee" && people[1].lastSeen == nil)
    // `revoked` is read when the host says it and is false otherwise.
    #expect(!sam.revoked && !people[1].revoked)
    let gone = try JSONDecoder().decode(HostPerson.self, from: Data(#"{"personId":"kay","revoked":true}"#.utf8))
    #expect(gone.revoked)
}

@Test func invitesCreatedInviteAndAuditDecode() throws {
    struct Invites: Decodable { var invites: [HostInvite] }
    let invites = try JSONDecoder().decode(Invites.self, from: Data(#"""
    {"invites":[{"inviteId":"inv_1","personId":"sam","agents":{"house":"guest"},"createdAt":1000,"expiresAt":9999999999999,"usedAt":null,"revokedAt":null},
                {"inviteId":"inv_2","personId":"sam","agents":{},"createdAt":1000,"expiresAt":2000,"usedAt":1500,"revokedAt":null}]}
    """#.utf8)).invites
    #expect(invites[0].isOpen())
    #expect(!invites[1].isOpen())
    #expect(!HostInvite(inviteId: "i", personId: "p", agents: [:], createdAt: 0, expiresAt: 5, usedAt: nil, revokedAt: nil).isOpen())
    #expect(!HostInvite(inviteId: "i", personId: "p", agents: [:], createdAt: 0, expiresAt: 9_999_999_999_999, usedAt: nil, revokedAt: 3).isOpen())

    let made = try JSONDecoder().decode(CreatedInvite.self, from: Data(
        #"{"inviteId":"inv_1","link":"kith://invite?c=comp&i=inv_1&t=aaaaaaaa","expiresAt":1700000000000,"personId":"sam"}"#.utf8))
    #expect(made.inviteId == "inv_1" && made.personId == "sam")
    #expect(made.expiry == Date(timeIntervalSince1970: 1_700_000_000))

    struct Audit: Decodable { var audit: [AuditRow] }
    let rows = try JSONDecoder().decode(Audit.self, from: Data(#"""
    {"audit":[{"id":2,"at":1700000000000,"personId":"sam","deviceKey":"k1","method":"send","target":"bot1","allowed":false},
              {"id":1,"at":1699999999000,"personId":"sam","deviceKey":"k1","method":"hello","target":null,"allowed":true}]}
    """#.utf8)).audit
    #expect(rows.map(\.allowed) == [false, true])
    #expect(rows[0].target == "bot1" && rows[1].target == nil)
}

@Test func cloudComputersCarryTheRoleAndAcceptDecodes() throws {
    struct Res: Decodable { var computers: [CloudComputer] }
    let rows = try JSONDecoder().decode(Res.self, from: Data(#"""
    {"computers":[{"computerId":"a","name":"Mine","signKey":"k","role":"owner"},
                  {"computerId":"b","name":"Theirs","signKey":"k","role":"member"},
                  {"computerId":"c","name":"Old cloud","signKey":"k"},
                  {"computerId":"d","name":"New role","signKey":"k","role":"supervisor"}]}
    """#.utf8)).computers
    // A role this version doesn't know reads as member (the contract's ComputerRole), so it gets member's rights.
    #expect(rows.map(\.role) == [.owner, .member, nil, .member])
    #expect(rows.map(\.isMember) == [false, true, false, true])
    let accepted = try JSONDecoder().decode(InviteAccepted.self, from: Data(#"{"computerId":"b","membershipId":"mem_1"}"#.utf8))
    #expect(accepted.computerId == "b" && accepted.membershipId == "mem_1")
}

@Test func anInvitedPersonsTrimmedHelloStillDecodes() throws {
    // roles.rs `shape`: hostId, computerId, signKey, boxKey, name, version, protocol, minProtocol, os, rev.
    // No backends, no paths.
    let json = #"{"hostId":"h1","computerId":"c1","name":"Mac","version":"0.1.0","protocol":1,"minProtocol":1,"os":"macos","rev":4}"#
    let hello = try JSONDecoder().decode(Hello.self, from: Data(json.utf8))
    #expect(hello.backends.isEmpty && hello.home == nil && hello.rev == 4)
    #expect(hello.protocolLevel == 1 && hello.minProtocol == 1)
    // A list of the wrong shape is still an error, which BotStore reads as "the host changed".
    #expect(throws: DecodingError.self) {
        _ = try JSONDecoder().decode(Hello.self, from: Data(#"{"hostId":"h","name":"M","version":"1","os":"macos","rev":0,"backends":"changed shape"}"#.utf8))
    }
}

@Test func inviteProofErrorsAreRecognised() {
    #expect(InviteProof.isAlreadyProven(HostError.http(400, "This device has already shown its invite.")))
    #expect(!InviteProof.isAlreadyProven(HostError.http(403, "That isn't this invite's token.")))
    #expect(InviteProof.isUnknownMethod(HostError.http(404, "unknown method")))
    #expect(!InviteProof.isUnknownMethod(HostError.http(403, "That isn't this invite's token.")))
}

@Test func grantedAgentsInTheMemberHelloDecodeAndAreOptional() throws {
    let base = #""hostId":"h1","name":"Mac","version":"3.0.0","os":"macos","rev":4"#
    let with = try JSONDecoder().decode(Hello.self, from: Data(
        #"{\#(base),"grantedAgents":[{"agent":"house","name":"House","role":"guest"},{"agent":"work","role":"member"},{"agent":"x","role":"odd"}]}"#.utf8))
    #expect(with.grantedAgents?.map(\.agent) == ["house", "work", "x"])
    #expect(with.grantedAgents?.map(\.role) == [.guest, .member, .guest])
    #expect(with.grantedAgents?.map(\.title) == ["House", "work", "x"])
    // An older host, or an owner's hello, says nothing; a malformed list is read as nothing too.
    #expect(try JSONDecoder().decode(Hello.self, from: Data("{\(base)}".utf8)).grantedAgents == nil)
    #expect(try JSONDecoder().decode(Hello.self, from: Data(#"{\#(base),"grantedAgents":"changed"}"#.utf8)).grantedAgents == nil)
}

// MARK: - requests

@Test func hostCallsUseTheHostsMethodsAndShapes() async throws {
    let transport = RecordingTransport()
    let client = HostClient(transport: transport)
    _ = try? await client.createInvite(name: "Sam", personId: nil, agents: ["house": .guest])
    _ = try? await client.createInvite(name: nil, personId: "sam", agents: ["work": .member])
    try? await client.setPersonAccess(personId: "sam", agents: ["house": .member], projects: ["site"])
    try? await client.proveInvite(inviteId: "inv_1", token: fakeToken)
    _ = try? await client.startChat(agent: "house")
    try? await client.unrevokePerson("sam")
    let calls = await transport.calls
    #expect(calls.map(\.method) == ["createInvite", "createInvite", "setPersonAccess", "proveInvite", "startChat", "unrevokePerson"])
    func json(_ i: Int) throws -> [String: Any] { try #require(JSONSerialization.jsonObject(with: calls[i].body) as? [String: Any]) }
    #expect(try json(5)["personId"] as? String == "sam")
    #expect(try json(0)["name"] as? String == "Sam")
    #expect(try json(0)["personId"] == nil)
    #expect(try json(0)["agents"] as? [String: String] == ["house": "guest"])
    #expect(try json(1)["personId"] as? String == "sam")
    #expect(try json(1)["name"] == nil)
    #expect(try json(2)["projects"] as? [String] == ["site"])
    #expect(try json(3)["inviteId"] as? String == "inv_1")
    #expect(try json(3)["token"] as? String == fakeToken)
    #expect(try json(4)["agent"] as? String == "house")
}

@Test func unregisterActivityNamesTheBot() async throws {
    let transport = RecordingTransport()
    try? await HostClient(transport: transport).unregisterActivity(botId: "bot_1")
    let calls = await transport.calls
    #expect(calls.map(\.method) == ["unregisterActivity"])
    let body = try #require(JSONSerialization.jsonObject(with: calls[0].body) as? [String: String])
    #expect(body == ["botId": "bot_1"])
}

private actor RecordingTransport: HostTransport {
    struct Call: Sendable { var method: String; var body: Data }
    private(set) var calls: [Call] = []

    nonisolated func call(_ method: String, body: Data, timeout: TimeInterval) async throws -> Data {
        await record(Call(method: method, body: body))
        throw HostError.unreachable
    }

    private func record(_ call: Call) { calls.append(call) }

    nonisolated func stream(_ request: HostStreamRequest) -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream { $0.finish() }
    }

    nonisolated func states() -> AsyncStream<LinkState> { AsyncStream { $0.finish() } }
}
