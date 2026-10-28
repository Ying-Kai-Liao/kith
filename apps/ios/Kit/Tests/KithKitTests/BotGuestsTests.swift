import Foundation
import Testing
@testable import KithKit

private let botJSON = #"""
{"botId":"bg_1","name":"Rex","provider":"grok","state":"pending","ownerPerson":"sam","ownerName":"Sam",
 "agents":["house","notes","finance","x"],"limits":{"ratePerHour":60,"concurrent":2},"connected":false,
 "createdAt":1000,"joinedAt":null,"decidedAt":null,"decidedBy":null,"lastSeenAt":null,"revokedAt":null,"revokedWhy":null,
 "personId":"bot-1","conversations":[{"conversation":"c1","agent":"house","chatBotId":null,"threadRoot":null,"originLabel":"Dinner","lastAt":5}]}
"""#

@Test func botRowDecodesWithNullableFields() throws {
    let bot = try JSONDecoder().decode(BotGuest.self, from: Data(botJSON.utf8))
    #expect(bot.state == .pending)
    #expect(bot.ownerName == "Sam" && bot.joinedAt == nil && bot.lastSeenAt == nil && bot.revokedAt == nil)
    #expect(bot.limits == BotGuestLimits(ratePerHour: 60, concurrent: 2))
    #expect(bot.conversations.first?.originLabel == "Dinner")
    #expect(bot.requestHeading == "Sam's bot · Grok")
    #expect(bot.requestQuestion == "Rex wants to ask house, notes and 2 more")
}

@Test func anUnknownStateDecodesInsteadOfFailing() throws {
    let json = #"{"botId":"b","name":"N","state":"frozen","agents":[]}"#
    let bot = try JSONDecoder().decode(BotGuest.self, from: Data(json.utf8))
    #expect(bot.state == .unknown("frozen"))
    #expect(!bot.state.isEnded)
}

@Test func askLineNamesUpToThreeAgents() {
    func asks(_ agents: [String]) -> String { BotGuest(botId: "b", name: "Rex", state: .pending, agents: agents).asks }
    #expect(asks(["house"]) == "house")
    #expect(asks(["house", "notes"]) == "house and notes")
    #expect(asks(["house", "notes", "finance"]) == "house, notes and finance")
    #expect(asks(["a", "b", "c", "d", "e"]) == "a, b and 3 more")
}

@Test func personCarriesAutoBots() throws {
    let on = try JSONDecoder().decode(HostPerson.self, from: Data(#"{"personId":"sam","name":"Sam","autoBots":true}"#.utf8))
    let old = try JSONDecoder().decode(HostPerson.self, from: Data(#"{"personId":"sam","name":"Sam"}"#.utf8))
    #expect(on.autoBots && !old.autoBots)
}

@Test func botEventsAreDecoded() {
    if case .botRequest = HostClient.parseEvent(Data(#"{"type":"botRequest","bot":{"botId":"b"}}"#.utf8)) {} else { Issue.record("botRequest") }
    if case .botUpdated = HostClient.parseEvent(Data(#"{"type":"botUpdated","bot":{"botId":"b","state":"active"}}"#.utf8)) {} else { Issue.record("botUpdated") }
}

@Test func membersAccessRequestsAreDecoded() {
    let json = #"{"type":"accessRequests","requests":[{"requestId":"req_1","deviceKey":"k","deviceName":"Kith web","platform":"web","email":"sam@x.com","personId":"sam","code":"123456","createdAt":1,"expiresAt":2}]}"#
    guard case let .accessRequests(list) = HostClient.parseEvent(Data(json.utf8)) else { Issue.record("accessRequests"); return }
    #expect(list.map(\.requestId) == ["req_1"])
    #expect(list.first?.spacedCode == "123 456")
    let waiting = MemberAccessRequest(requestId: "r", deviceName: "d", platform: "web", expiresAt: 0)
    #expect(waiting.spacedCode == nil)
}

private actor Canned: HostTransport {
    struct Call: Sendable { var method: String; var body: Data }
    private(set) var calls: [Call] = []
    let answer: String
    init(_ answer: String) { self.answer = answer }

    nonisolated func call(_ method: String, body: Data, timeout: TimeInterval) async throws -> Data {
        await record(Call(method: method, body: body))
        return Data(answer.utf8)
    }

    private func record(_ call: Call) { calls.append(call) }
    nonisolated func stream(_ request: HostStreamRequest) -> AsyncThrowingStream<Data, Error> { AsyncThrowingStream { $0.finish() } }
    nonisolated func states() -> AsyncStream<LinkState> { AsyncStream { $0.finish() } }
}

private func body(_ call: Canned.Call) throws -> [String: Any] {
    try #require(JSONSerialization.jsonObject(with: call.body) as? [String: Any])
}

@Test func createBotInviteSendsTheFormAndReturnsTheHostsShareText() async throws {
    let transport = Canned(#"{"botId":"bg_1","inviteId":"bi_1","expiresAt":9,"link":"https://x/b/1","kithLink":"kith://bot?c=1","share":"Install the Kith client\n"}"#)
    let made = try await HostClient(transport: transport).createBotInvite(name: "Rex", provider: "grok", agents: ["house"])
    #expect(made.share == "Install the Kith client\n")
    #expect(made.inviteId == "bi_1" && made.expiresAt == 9)
    let call = try #require(await transport.calls.first)
    #expect(call.method == "createBotInvite")
    let sent = try body(call)
    #expect(sent["name"] as? String == "Rex" && sent["provider"] as? String == "grok" && sent["agents"] as? [String] == ["house"])
}

@Test func newLinkSendsOnlyTheBotId() async throws {
    let transport = Canned(#"{"botId":"bg_1","inviteId":"bi_2","expiresAt":9,"link":"l","share":"s"}"#)
    _ = try await HostClient(transport: transport).createBotInvite(name: nil, provider: nil, agents: [], botId: "bg_1")
    let sent = try body(try #require(await transport.calls.first))
    #expect(sent["botId"] as? String == "bg_1" && sent["name"] == nil)
}

@Test func listsAndAnswersUseTheHostsNames() async throws {
    let list = Canned("{\"bots\":[\(botJSON)]}")
    let client = HostClient(transport: list)
    #expect(try await client.bots().first?.botId == "bg_1")
    #expect(try await client.myBots().count == 1)
    #expect(await list.calls.map(\.method) == ["bots", "myBots"])

    let ok = Canned(#"{"bot":null}"#)
    let c2 = HostClient(transport: ok)
    try await c2.decideBot(botId: "bg_1", decision: .allow, alwaysForOwner: true)
    try await c2.setAutoAllowBots(personId: "sam", on: true)
    try await c2.revokeBot(botId: "bg_1")
    let calls = await ok.calls
    #expect(calls.map(\.method) == ["decideBot", "setAutoAllowBots", "revokeBot"])
    let decide = try body(calls[0])
    #expect(decide["decision"] as? String == "allow" && decide["alwaysForOwner"] as? Bool == true)
    #expect(try body(calls[1])["on"] as? Bool == true)
}
