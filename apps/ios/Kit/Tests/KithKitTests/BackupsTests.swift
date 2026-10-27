import Foundation
import Testing
@testable import KithKit

private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try JSONDecoder().decode(T.self, from: Data(json.utf8))
}

@Test func backupsDecodeAndOddEntriesDropOut() throws {
    let b = try decode(Backups.self, """
    {"global":["codex","claude:work"],"agents":{"house":["claude:work"]},"future":1,
     "entries":[{"id":"codex","ready":false,"why":"not logged in","fix":"codex login"},
                {"id":"claude:work","ready":true,"outUntil":2000000,"reason":"rate_limit"},
                {"nope":true}, 5]}
    """)
    #expect(b.global == ["codex", "claude:work"] && b.agents == ["house": ["claude:work"]])
    #expect(b.entries.map(\.id) == ["codex", "claude:work"])
    #expect(b.entry("codex").fix == "codex login")
    #expect(b.entry("claude:work").outUntil == Date(timeIntervalSince1970: 2000))
    #expect(b.entry("unlisted").ready == false, "an entry the host didn't describe isn't claimed ready")
    #expect(b.globalText == "codex, claude:work")
}

@Test func emptyOrOddAnswersAreAnEmptyChain() throws {
    for json in ["{}", #"{"global":5,"entries":"x"}"#] {
        let b = try decode(Backups.self, json)
        #expect(b.global.isEmpty && b.agents.isEmpty && b.entries.isEmpty)
    }
}

@Test func stateSaysReadyOutOrWhy() throws {
    let now = Date(timeIntervalSince1970: 1000)
    #expect(BackupEntry(id: "codex").state(now: now) == "ready")
    #expect(BackupEntry(id: "codex", ready: false, why: "not logged in").state(now: now) == "not logged in")
    let out = BackupEntry(id: "claude:work", outUntil: Date(timeIntervalSince1970: 1060), reason: "rate_limit")
    #expect(out.isOut(now: now) && out.state(now: now).hasPrefix("out of usage, back at "))
    #expect(!out.isOut(now: Date(timeIntervalSince1970: 1100)), "back once the time has passed")
    #expect(BackupEntry(id: "x", ready: false).state(now: now) == "not ready")
}

@Test func theBadgeNamesTheEntryThatAnswered() {
    #expect(backupViaLabel("codex") == "via codex")
    #expect(backupViaLabel("claude:work") == "via claude:work")
    #expect(backupViaLabel(nil) == nil && backupViaLabel("") == nil)
}

@Test func entryDataKeepsAnsweredBy() throws {
    let data = try decode(EntryData.self, #"{"text":"hi","final":true,"answeredBy":"codex"}"#)
    #expect(data.answeredBy == "codex")
    #expect(try decode(EntryData.self, #"{"text":"hi","answeredBy":null}"#).answeredBy == nil)
    #expect(try decode(EntryData.self, #"{"text":"old message"}"#).answeredBy == nil)
}

@Test func entryDataKeepsContextPercent() throws {
    #expect(try decode(EntryData.self, #"{"text":"hi","contextPercent":42}"#).contextPercent == 42)
    #expect(try decode(EntryData.self, #"{"text":"hi","contextPercent":120}"#).contextPercent == 120)
    #expect(try decode(EntryData.self, #"{"text":"old message"}"#).contextPercent == nil)
}

@Test func mainAccountsDecodeAboveTheChainAndOldHostsHaveNone() throws {
    let b = try decode(Backups.self, """
    {"global":["codex"],"entries":[],
     "main":{"global":"claude:work","agents":{
        "house":{"id":"claude:work","source":"global","harness":"claude","ready":true},
        "deep":{"id":"codex:deep","source":"agent","ready":false,"why":"not logged in","fix":"kith account login codex deep"},
        "bad":{"source":"agent"}, "worse":5}}}
    """)
    #expect(b.mainGlobal == "claude:work")
    #expect(Set(b.mains.keys) == ["house", "deep"], "an odd agent row drops out alone")
    #expect(b.mains["house"]?.state == "ready")
    #expect(b.mains["deep"]?.state == "not logged in" && b.mains["deep"]?.fix == "kith account login codex deep")
    #expect(b.ownMains.map(\.agent) == ["deep"], "only an agent's own choice is listed under the computer's")
    let old = try decode(Backups.self, #"{"global":["codex"],"entries":[]}"#)
    #expect(old.mainGlobal == nil && old.mains.isEmpty)
    #expect(try decode(Backups.self, #"{"main":{"global":null,"agents":{}}}"#).mainGlobal == nil)
}
