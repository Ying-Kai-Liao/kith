import Foundation
import Testing
@testable import KithKit
@testable import KithUI

private func bot(_ json: String) throws -> Bot { try JSONDecoder().decode(Bot.self, from: Data(json.utf8)) }

private let installed = [("pdf", "PDF"), ("code-review", "Code Review"), ("xlsx", "xlsx")].map {
    InstalledSkill(id: $0.0, name: $0.1, description: "", source: "custom", path: "")
}

@Test func aLeadingSlashOpensTheListUntilTheNameEnds() {
    #expect(SlashCommands.query("/") == "")
    #expect(SlashCommands.query("/pd") == "pd")
    #expect(SlashCommands.query("/pdf merge") == nil)
    #expect(SlashCommands.query("hi /pdf") == nil)
}

@Test func theListHoldsOnlyTheBotsOwnSkills() throws {
    let own = try bot(#"{"id":"b","skills":["pdf","code-review"]}"#)
    #expect(SlashCommands.list(for: own, query: "", skills: installed).map(\.name) == ["pdf", "code-review"])
    #expect(SlashCommands.list(for: own, query: "review", skills: installed).map(\.name) == ["code-review"])
    #expect(SlashCommands.list(for: own, query: "fast", skills: installed).isEmpty, "Fast mode is gone")
    #expect(SlashCommands.list(for: try bot(#"{"id":"b","skills":["pdf"]}"#), query: "xl", skills: installed).isEmpty)
    #expect(SlashCommands.list(for: try bot(#"{"id":"b"}"#), query: "", skills: []).isEmpty)
    #expect(SlashCommands.list(for: try bot(#"{"id":"g","kind":"group"}"#), query: "", skills: installed).isEmpty)
}
