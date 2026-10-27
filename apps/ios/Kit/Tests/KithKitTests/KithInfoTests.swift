import Foundation
import Testing
@testable import KithKit

private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try JSONDecoder().decode(T.self, from: Data(json.utf8))
}

/// A real `kith house --info-json` (memory files trimmed to three).
private func houseInfo() throws -> KithInfo {
    let url = try #require(Bundle.module.url(forResource: "kith-house-info", withExtension: "json", subdirectory: "Fixtures"))
    return try JSONDecoder().decode(KithInfo.self, from: Data(contentsOf: url))
}

@Test func realHouseInfoDecodes() throws {
    let info = try houseInfo()
    #expect(info.agent == "house" && info.harness == "claude" && info.permissionMode == "auto" && info.effort == "low")
    #expect(info.package?.chip == "v393def5")
    #expect(info.package?.dirty == false && info.package?.pinned == nil)
    #expect(info.role?.sections.map(\.title) == ["How you work", "Your skills", "Not yours"])
    #expect(info.role?.sections.last?.isNotYours == true)
    #expect(info.skills.count == 4 && info.skills.allSatisfy { $0.source == "package" })
    #expect(info.tools.count == 3 && info.mcpServers.isEmpty && info.plugins == ["kith@kith"])
    #expect(info.schedules.isEmpty && info.projects.isEmpty && info.errors.isEmpty)
    let cost = try #require(info.cost)
    #expect(cost.requestsMonth == 146 && KithCost.format(cost.today) == "$0.12")
    #expect(cost.topModels.map(\.model) == ["claude-sonnet-5-5", "claude-opus-5-5", "claude-haiku-5-5"])
    #expect(abs(cost.topModels[0].share - 1.3776 / 2.7219) < 0.001)
    let memory = try #require(info.memory)
    #expect(memory.files.count == 3 && memory.totalBytes == 3920)
    #expect(memory.ordered.first?.path == "MEMORY.md" && memory.ordered[1].path == "sessions/house.md")
}

@Test func infoToleratesMissingAndUnknownKeys() throws {
    let info = try decode(KithInfo.self, #"{"agent":"a","future":{"x":1},"skills":"nonsense","schedules":[{"id":"s"},42],"cost":null}"#)
    #expect(info.agent == "a" && info.skills.isEmpty && info.cost == nil && info.memory == nil && info.errors.isEmpty)
    // One odd element drops out alone.
    #expect(info.schedules.map(\.id) == ["s"])
}

@Test func responseKeepsTheErrorWhenThereIsNoInfo() throws {
    let down = try decode(KithInfoResponse.self, #"{"info":null,"error":"kith: timed out","fetchedAt":1760000000000}"#)
    #expect(down.info == nil && down.error == "kith: timed out" && down.fetchedAt == 1_760_000_000_000)
    let none = try decode(KithInfoResponse.self, #"{"info":null}"#)
    #expect(none.info == nil && none.error == nil)
    let partial = try decode(KithInfoResponse.self, #"{"info":{"agent":"a","errors":{"cost":"no ledger"}},"fetchedAt":1}"#)
    #expect(partial.info?.errors["cost"] == "no ledger")
}

@Test func scheduleReadsStatusDatesAndPlainCron() throws {
    let job = try decode(KithSchedule.self, #"{"id":"j","cron":"0 9 * * 1-5","task":"Check bills","paused":true,"last_run":"2026-10-21T09:00:03+11:00","last_status":"running","next_run":null}"#)
    #expect(job.paused && job.isRunning && job.lastRun != nil && job.nextRun == nil)
    #expect(job.plainCron == "Weekdays at 09:00")
    #expect(KithSchedule.describe("30 7 * * *") == "Daily at 07:30")
    #expect(KithSchedule.describe("*/15 * * * *") == "Every 15 minutes")
    #expect(KithSchedule.describe("0 * * * *") == "Hourly")
    #expect(KithSchedule.describe("0 9 1 * *") == "0 9 1 * *")
    #expect(KithSchedule.describe("nonsense") == "nonsense")
}

@Test func costFormatsTinyAmounts() {
    #expect(KithCost.format(0) == "$0.00" && KithCost.format(0.001) == "<$0.01" && KithCost.format(2.7219) == "$2.72")
}

@Test func memoryFileAndSearchDecode() throws {
    let file = try decode(KithMemoryFile.self, #"{"path":"notes.md","text":"hi","bytes":19,"modified":1760000000000,"truncated":true}"#)
    #expect(file.truncated && file.text == "hi" && file.modified?.date == Date(timeIntervalSince1970: 1_760_000_000))
    let search = try decode(KithSearchResult.self, #"{"hits":[{"path":"notes.md","line":3,"snippet":"milk","openable":true},{"path":"/abs/skill.md","line":null,"snippet":"x","openable":false,"agent":"finance"}]}"#)
    #expect(search.hits.count == 2 && search.hits[0].openable && search.hits[1].line == nil && search.hits[1].agent == "finance")
    let failed = try decode(KithSearchResult.self, #"{"hits":[],"error":"kith failed"}"#)
    #expect(failed.hits.isEmpty && failed.error == "kith failed")
}

@Test func mcpServerShowsCommandOrHost() throws {
    #expect(try decode(KithMCPServer.self, #"{"name":"a","kind":"stdio","command":"npx x","url":null}"#).target == "npx x")
    #expect(try decode(KithMCPServer.self, #"{"name":"b","kind":"http","url":"https://mcp.example.com/v1"}"#).target == "mcp.example.com")
}

@Test func botReadsHqAgentOnlyWhenSent() throws {
    #expect(try decode(Bot.self, #"{"id":"a","hqAgent":"house"}"#).hqAgent == "house")
    #expect(try decode(Bot.self, #"{"id":"a"}"#).hqAgent == nil)
}

@Test func hqBotWithKithDetailsHidesTheCardsKithRepeats() throws {
    let sections = BotSections(hqAgent: "house", info: try houseInfo())
    #expect(!sections.instructions && !sections.backendPicker && !sections.harnessMemoryTab && !sections.kithSettings)
}

@Test func plainBotAndUnloadedKithKeepEveryCard() throws {
    #expect(BotSections(hqAgent: nil, info: try houseInfo()) == .all)
    #expect(BotSections(hqAgent: "", info: try houseInfo()) == .all)
    var unloaded = BotSections.all
    unloaded.harnessMemoryTab = false
    unloaded.kithSettings = false
    #expect(BotSections(hqAgent: "house", info: nil) == unloaded)
}

@Test func aCardStaysWhenKithCouldNotReadWhatItRepeats() throws {
    var info = try houseInfo()
    info.harness = nil
    info.errors = ["role": "unreadable"]
    let sections = BotSections(hqAgent: "house", info: info)
    #expect(sections.instructions && sections.backendPicker && !sections.harnessMemoryTab)
}

@Test func rolePromptJoinsIntroAndEverySection() throws {
    let role = try #require(try houseInfo().role)
    let prompt = try #require(role.prompt)
    #expect(prompt.hasPrefix(role.intro.trimmingCharacters(in: .whitespacesAndNewlines)))
    #expect(role.sections.allSatisfy { prompt.contains("## \($0.title)") })
    let empty = try decode(KithRole.self, #"{"intro":" ","sections":[]}"#)
    #expect(empty.prompt == nil)
}

@Test func anHqBotOnAHostWithoutKithInfoKeepsKithsOwnSettingsControls() throws {
    #expect(BotSections(hqAgent: "house", info: nil, unsupportedHost: true).kithSettings)
    #expect(BotSections(hqAgent: "house", info: try houseInfo(), unsupportedHost: true).kithSettings)
    #expect(!BotSections(hqAgent: "house", info: try houseInfo()).kithSettings)
    #expect(BotSections(hqAgent: nil, info: nil).kithSettings)
}

@Test func hqSettingsWriteThePermissionThroughAndTheRestIsReadOnlySetInKith() throws {
    var info = try houseInfo()
    info.permissionMode = "plan"
    info.model = "opus"
    info.effort = nil
    let rows = HqSetting.rows(info)
    #expect(rows.map(\.key) == [.permission, .callModel, .model, .effort, .fastMode])
    #expect(rows.map(\.value) == ["plan", "default", "opus", "Agent's default", "Off"])
    #expect(rows.map(\.editable) == [true, true, false, false, false])
    #expect(rows.filter { !$0.editable }.allSatisfy { $0.hint.hasPrefix(HqSetting.setInKith) })
}

@Test func nothingIsEditableUntilHqAnsweredAndNoPermissionInHqShowsTheDefault() throws {
    #expect(HqSetting.rows(nil)[0].editable == false)
    var info = try houseInfo()
    info.permissionMode = nil
    #expect(HqSetting.rows(info)[0].value == "auto")
    #expect(HqSetting.permissionLabel("bypassPermissions") == "Bypass permissions")
    #expect(HqSetting.permissionLabel("somethingNew") == "somethingNew")
}

@Test func permissionPickerHoldsClaudeCodesSixModesAndOldValuesReadAsDefault() throws {
    #expect(HqSetting.permissionModes.map(\.id) == ["default", "acceptEdits", "plan", "auto", "bypassPermissions"])
    #expect(HqSetting.permissionMode("ask") == "default" && HqSetting.permissionMode("manual") == "default")
    #expect(HqSetting.permissionMode("auto") == "auto")
    var info = try houseInfo()
    info.permissionMode = "manual"
    #expect(HqSetting.rows(info)[0].value == "default")
}

@Test func theCallModelShowsHqsChoiceAndAnUnknownOneAsItIs() throws {
    var info = try houseInfo()
    info.callModel = "haiku"
    #expect(HqSetting.rows(info)[1].value == "haiku")
    #expect(HqSetting.callModelLabel("default") == "Default (sonnet)")
    #expect(HqSetting.callModelLabel("default", defaultIs: "opus") == "Default (opus)")
    #expect(HqSetting.callModelLabel("own") == "Agent's own model")
    #expect(HqSetting.callModelLabel("gpt-x") == "gpt-x")
    #expect(HqSetting.rows(nil)[1].editable == false)
}

@Test func accessLedgerDecodesAndSaysWhatEachAllows() throws {
    let answer = try decode(AccessLedger.self, """
    {"grants": [
      {"key": "read:house:sam:/Users/kai/docs", "kind": "read", "agent": "house", "person": "sam", "name": "Sam", "target": "/Users/kai/docs", "since": 1790000000000, "approved_by": null},
      {"key": "domain:house:*:pypi.org", "kind": "domain", "agent": "house", "person": "*", "name": "", "target": "pypi.org", "since": null},
      {"key": "write:house:sam:/Users/kai/notes", "kind": "write", "agent": "house", "person": "sam", "target": "/Users/kai/notes"},
      {"key": "service:house:sam:notion/sam", "kind": "service", "agent": "house", "person": "sam", "target": "notion/sam"},
      {"key": "exec:house:sam:/bin", "kind": "exec", "person": "sam", "target": "/bin"},
      {"kind": "read", "person": "sam", "target": "/x"}
    ]}
    """)
    #expect(answer.grants.map(\.text) == ["Read /Users/kai/docs", "Connect to pypi.org", "Read and write /Users/kai/notes", "Use notion/sam"])
    #expect(answer.grants.map(\.id) == answer.grants.map(\.key))
    #expect(answer.grants[0].agent == "house")
    #expect(answer.grants[0].holder.hasPrefix("Sam · since "))
    #expect(answer.grants[1].holder == "Everyone")
    #expect(answer.grants[2].holder == "sam")
    #expect(try decode(AccessLedger.self, "{}").grants.isEmpty)
}

@Test func aPermissionEntryCarriesTheHostsOutcome() throws {
    let data = try decode(EntryData.self, #"{"status": "answered", "selected": "allow", "outcome": "Allowed once only: it can't be kept for always (x)"}"#)
    #expect(data.outcome == "Allowed once only: it can't be kept for always (x)")
    #expect(try decode(EntryData.self, #"{"status": "answered"}"#).outcome == nil)
}
