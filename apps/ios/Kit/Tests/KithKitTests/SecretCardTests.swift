import Foundation
import Testing
@testable import KithKit

private func entry(_ data: String) throws -> Entry {
    let json = #"{"id":"e1","botId":"b","kind":"permission","seq":1,"rev":1,"turn":1,"createdAt":1,"updatedAt":1,"data":\#(data)}"#
    return try JSONDecoder().decode(Entry.self, from: Data(json.utf8))
}

@Test func anAccessCardDecodesItsSecretAndAccounts() throws {
    let e = try entry(#"{"status":"pending","card":"account","secret":{"service":"github","vars":["GITHUB_TOKEN"],"profile":null,"account":null,"purpose":"Open a PR","replaces":false,"state":"pending"},"accounts":[{"profile":"github/work","account":"work","agents":["coder"]},{"profile":"github/personal","account":"personal","agents":[]}]}"#)
    #expect(e.data.isSecretCard && e.data.card == "account")
    #expect(e.data.secret?.service == "github" && e.data.secret?.pending == true && e.data.secret?.profile == nil)
    #expect(e.data.accounts?.map(\.profile) == ["github/work", "github/personal"])
    #expect(e.data.accounts?.first?.agents == ["coder"])
}

@Test func aKeyCardDecodesAReplace() throws {
    let e = try entry(#"{"status":"pending","card":"key","secret":{"service":"notion","vars":["NOTION_TOKEN"],"profile":"notion/team","account":"team","replaces":true,"state":"saved"}}"#)
    #expect(e.data.card == "key" && e.data.secret?.replaces == true && e.data.secret?.state == "saved" && e.data.secret?.pending == false)
    #expect(try JSONDecoder().decode(EntryData.self, from: JSONEncoder().encode(e.data)) == e.data)
}

@Test func entriesWithoutACardAreOrdinary() throws {
    let e = try entry(#"{"status":"pending","title":"swift test"}"#)
    #expect(!e.data.isSecretCard && e.data.secret == nil && e.data.accounts == nil)
    #expect(!(try entry(#"{"card":"something-new"}"#)).data.isSecretCard)
}

@Test func keyRecordsDecodeAndGroupByService() throws {
    let json = #"{"keys":[{"profile":"github/work","service":"github","account":"work","owner":"kai","vars":["GITHUB_TOKEN"],"agents":["coder"],"last_used":"2026-10-25T08:12:00Z"},{"profile":"notion/team","service":"notion","account":"team","owner":"kai","vars":["NOTION_TOKEN"],"agents":[],"last_used":null},{"profile":"github/home","service":"github","account":"home","vars":[],"agents":[],"last_used":null}]}"#
    struct Res: Decodable { var keys: [KeyRecord] }
    let keys = try JSONDecoder().decode(Res.self, from: Data(json.utf8)).keys
    #expect(keys[0].lastUsedDate != nil && keys[1].lastUsedDate == nil)
    let groups = KeyRecord.grouped(keys)
    #expect(groups.map(\.service) == ["github", "notion"])
    #expect(groups[0].keys.map(\.account) == ["home", "work"])
}

@Test func accountNamesFollowTheHostsRule() {
    for ok in ["work", "a", "my-team.2", "0abc", String(repeating: "a", count: 40)] { #expect(SecretRules.validAccount(ok), "\(ok)") }
    for bad in ["", "-x", ".x", "Work", "a b", "a/b", String(repeating: "a", count: 41)] { #expect(!SecretRules.validAccount(bad), "\(bad)") }
}

@Test func keyValuesAreOneToFifteenThirtySixBytesWithoutNul() {
    #expect(SecretRules.validValue("x") && SecretRules.validValue(String(repeating: "x", count: 1536)))
    #expect(!SecretRules.validValue("") && !SecretRules.validValue(String(repeating: "x", count: 1537)) && !SecretRules.validValue("a\u{0}b"))
}
