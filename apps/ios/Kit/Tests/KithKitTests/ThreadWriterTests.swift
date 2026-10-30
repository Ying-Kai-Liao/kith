import Foundation
import Testing
@testable import KithKit

private func summary(_ json: String) throws -> ThreadSummary {
    try JSONDecoder().decode(ThreadSummary.self, from: Data(json.utf8))
}

private let base = #""count":2,"lastAt":5,"authors":["b"]"#

@Test func anOldSummaryDecodesWithNoWriterFields() throws {
    let s = try summary("{\(base)}")
    #expect(s.origin == nil && s.liveWriter == nil && s.terminal == nil)
    #expect(s.threadOrigin == .app)
    #expect(!s.isTerminalOrigin && !s.isHeldElsewhere)
}

@Test func aTerminalSummaryDecodesAndSurvivesTheCache() throws {
    let s = try summary(#"{\#(base),"origin":"terminal","originLabel":"from terminal","liveWriter":"terminal","liveBusy":true,"terminal":{"sessionId":"s1","cwd":"/w"}}"#)
    #expect(s.isTerminalOrigin && s.isHeldElsewhere && s.isHeldByTerminal)
    #expect(s.liveBusy == true && s.terminal?.cwd == "/w" && s.originLabel == "from terminal")
    let again = try JSONDecoder().decode(ThreadSummary.self, from: JSONEncoder().encode(s))
    #expect(again == s)
}

@Test func anAppThreadOpenedInATerminalIsHeldButNotTerminalOrigin() throws {
    let s = try summary(#"{\#(base),"liveWriter":"terminal"}"#)
    #expect(!s.isTerminalOrigin && s.isHeldElsewhere)
    #expect(try summary(#"{\#(base),"liveWriter":"app"}"#).isHeldElsewhere == false)
}

@Test func unknownOriginsAndWritersDecodeAndAreHeld() throws {
    let s = try summary(#"{\#(base),"origin":"matrix","liveWriter":"bot","originBot":{"provider":"p","owner":"o","name":"n"}}"#)
    #expect(s.threadOrigin == .other("matrix"))
    #expect(s.isHeldElsewhere)
    #expect(s.originBot?.name == "n")
    #expect(try summary(#"{\#(base),"origin":"discord"}"#).threadOrigin == .discord)
}

@Test func theResumeCommandNamesTheRootEntry() {
    let root = Entry(id: "e_1", seq: 0, botId: "b", threadId: nil, rev: 1, kind: "user", turn: 1, data: EntryData(), createdAt: 0, updatedAt: 0)
    #expect(root.resumeCommand(agent: "house") == "kith house --resume e_1")
}

@Test func writerChangeNoticesDecode() throws {
    let d = try JSONDecoder().decode(EntryData.self, from: Data(#"{"style":"divider","writerChange":"tookOver","at":9}"#.utf8))
    #expect(d.writerChangeKind == .tookOver && d.at == 9 && d.style == "divider")
    #expect(try JSONDecoder().decode(EntryData.self, from: Data(#"{"writerChange":"later"}"#.utf8)).writerChangeKind == .other("later"))
    #expect(EntryData().writerChangeKind == nil)
}

@Test func hostRefusalsBecomeTypedErrors() {
    #expect(ThreadWriterError(HostError.http(409, "liveInTerminal: Take it over.")) == .liveInTerminal("liveInTerminal: Take it over."))
    #expect(ThreadWriterError(HostError.http(409, "terminalBusy: Wait.")) == .terminalBusy("terminalBusy: Wait."))
    #expect(ThreadWriterError(HostError.http(409, "heldByOtherWriter: x")) != nil)
    #expect(ThreadWriterError(HostError.http(409, "paused")) == nil)
    #expect(ThreadWriterError(HostError.http(400, "liveInTerminal: x")) == nil)
    #expect(ThreadWriterError.terminalBusy("terminalBusy: Wait.").errorDescription == "Wait.")
}

private actor Canned: HostTransport {
    var reply: Result<Data, Error>
    var methods: [String] = []
    init(_ reply: Result<Data, Error>) { self.reply = reply }
    func call(_ method: String, body: Data, timeout: TimeInterval) async throws -> Data {
        methods.append(method)
        return try reply.get()
    }
    nonisolated func stream(_ request: HostStreamRequest) -> AsyncThrowingStream<Data, Error> { .init { $0.finish() } }
    nonisolated func states() -> AsyncStream<LinkState> { .init { $0.finish() } }
}

@Test func takeOverThreadReturnsTheResultAndMapsBusy() async throws {
    let ok = Canned(.success(Data(#"{"liveWriter":null,"tookOver":true}"#.utf8)))
    let result = try await HostClient(transport: ok).takeOverThread(botId: "b", threadId: "t")
    #expect(result.tookOver && result.liveWriter == nil)
    #expect(await ok.methods == ["takeOverThread"])

    let busy = Canned(.failure(HostError.http(409, "terminalBusy: The terminal is working.")))
    await #expect(throws: ThreadWriterError.self) { try await HostClient(transport: busy).takeOverThread(botId: "b", threadId: "t") }
}

@Test func sendToAHeldThreadThrowsLiveInTerminal() async {
    let held = Canned(.failure(HostError.http(409, "liveInTerminal: Open in a terminal.")))
    do {
        _ = try await HostClient(transport: held).send(botId: "b", text: "hi", clientNonce: "n", threadId: "t")
        Issue.record("should have thrown")
    } catch {
        #expect(error as? ThreadWriterError == .liveInTerminal("liveInTerminal: Open in a terminal."))
    }
}

@Test func aHeldThreadsPromptDoesNotNeedYou() throws {
    func root(_ json: String) throws -> Entry {
        var e = Entry(id: "t", seq: 0, botId: "b", threadId: nil, rev: 1, kind: "user", turn: 1, data: EntryData(), createdAt: 0, updatedAt: 0)
        e.data.thread = try summary(json)
        return e
    }
    let bot = try JSONDecoder().decode(Bot.self, from: Data(#"{"id":"b","workingLanes":[{"chat":"b","thread":"t"}]}"#.utf8))
    var prompt = Entry(id: "p", seq: 1, botId: "b", threadId: "t", rev: 1, kind: "permission", turn: 1, data: EntryData(), createdAt: 0, updatedAt: 0)
    prompt.data.status = "pending"
    #expect(Tasks.status(of: try root("{\(base)}"), bot: bot, replies: [prompt]) == .needsYou)
    #expect(Tasks.status(of: try root(#"{\#(base),"liveWriter":"terminal"}"#), bot: bot, replies: [prompt]) == .working)
}

@Test func aMirroredOneShotSaysWhoStartedItAndOtherThreadsSayNothing() throws {
    #expect(try summary(#"{\#(base),"origin":"terminal","originLabel":" asked by architect "}"#).runLabel == "asked by architect")
    #expect(try summary(#"{\#(base),"origin":"terminal"}"#).runLabel == nil)
    #expect(try summary(#"{\#(base),"origin":"terminal","originLabel":"  "}"#).runLabel == nil)
    #expect(try summary(#"{\#(base),"origin":"discord","originLabel":"Sam's server"}"#).runLabel == nil)
}

@Test func aQueuedMessageSaysWhatItWaitsFor() {
    #expect(UserStatusLine.queued(waitsFor: "terminal", botWorking: false) == "Waits until the terminal session ends")
    #expect(UserStatusLine.queued(waitsFor: "terminal", botWorking: true) == "Waits until the terminal session ends")
    #expect(UserStatusLine.queued(waitsFor: nil, botWorking: true) == "Queued until this response finishes")
    #expect(UserStatusLine.queued(waitsFor: nil, botWorking: false) == "Queued")
}
