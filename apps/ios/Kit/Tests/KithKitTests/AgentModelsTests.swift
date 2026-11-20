import Foundation
import Testing
@testable import KithKit

@Test func modelCatalogDecodesAgentNamesAndIDs() throws {
    let data = Data(#"{"models":[{"id":"opus[1m]","name":"Agent model","description":"From the agent"}],"currentModelId":"opus[1m]"}"#.utf8)
    let catalog = try JSONDecoder().decode(AgentModels.self, from: data)
    #expect(catalog.models.first?.id == "opus[1m]")
    #expect(catalog.models.first?.name == "Agent model")
    #expect(catalog.currentModelId == "opus[1m]")
}

@Test func modelDefaultExplicitlyClearsSavedOverride() throws {
    var draft = BotDraft(name: "Example", cwd: "/tmp")
    draft.id = "bot-id"
    let data = try JSONEncoder().encode(BotUpdate(draft: draft))
    let body = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
    #expect(body["model"] is NSNull)
    #expect(body["id"] as? String == "bot-id")
    #expect(body["name"] as? String == "Example")
    draft.model = "agent-model"
    let selected = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(BotUpdate(draft: draft))) as? [String: Any])
    #expect(selected["model"] as? String == "agent-model")
}

@Test func modelOptionsDecodeAndRoundTripThroughDrafts() throws {
    let data = Data(#"{"models":[{"id":"grok-4.7","name":"Grok 4.7","options":[{"id":"fast","name":"Fast","default":"true","values":[{"id":"false","name":"Off"},{"id":"true","name":"On"}]}]},{"id":"old","name":"Old host"}]}"#.utf8)
    let catalog = try JSONDecoder().decode(AgentModels.self, from: data)
    #expect(catalog.models.first?.options?.first?.values.map(\.name) == ["Off", "On"])
    #expect(catalog.models.last?.options == nil)
    let bot = try JSONDecoder().decode(Bot.self, from: Data(#"{"id":"b","modelOptions":{"fast":"false"}}"#.utf8))
    #expect(bot.modelOptions == ["fast": "false"])
    let body = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(BotUpdate(draft: BotDraft(bot)))) as? [String: Any])
    #expect(body["modelOptions"] as? [String: String] == ["fast": "false"])
}

@Test func aSingleSettingUpdateSendsOnlyTheIdAndThatKey() throws {
    let on = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(BotSetting(id: "b", key: "pinned", value: true))) as? [String: Any])
    #expect(Set(on.keys) == ["id", "pinned"])
    #expect(on["pinned"] as? Bool == true)
    let off = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(BotSetting(id: "b", key: "harnessMemory", value: Bool?.some(false)))) as? [String: Any])
    #expect(Set(off.keys) == ["id", "harnessMemory"])
}

@Test func cancelledTasksAreNotFailures() {
    #expect(CancellationError().isCancellation)
    #expect(URLError(.cancelled).isCancellation)
    #expect(!URLError(.timedOut).isCancellation)
}

@Test func anOlderHostsFastModeFieldsAreIgnored() throws {
    let catalog = try JSONDecoder().decode(AgentModels.self, from: Data(#"{"models":[],"fastMode":true}"#.utf8))
    #expect(catalog.models.isEmpty)
    let bot = try JSONDecoder().decode(
        Bot.self, from: Data(#"{"id":"b","backend":"codex","fastMode":true,"fastModeSupport":"offered","fastModeIssue":null}"#.utf8))
    #expect(bot.backend == "codex")
    let body = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(BotUpdate(draft: BotDraft(bot)))) as? [String: Any])
    #expect(body["fastMode"] == nil, "an update never sends the switch")
}
