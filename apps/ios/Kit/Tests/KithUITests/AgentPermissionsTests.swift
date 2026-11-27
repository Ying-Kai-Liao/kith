import KithKit
import Testing
@testable import KithUI

private let rules: [HostContract.AgentRule] = [
    .init(list: "allow", rule: "Read(~/docs/**)", source: "role/settings.json"),
    .init(list: "ask", rule: "Bash(git push:*)", source: "role/settings.json"),
    .init(list: "deny", rule: "Read(~/.ssh/**)", source: "role/settings.json"),
    .init(list: "allow", rule: "Bash(ls /tmp)", source: "allowed.json", id: "a1b2", kind: "rule"),
]

@Test func rulesGroupByList() {
    let groups = AgentPermissions.groups(rules: rules)
    #expect(groups.map(\.id) == ["allow", "ask", "deny"])
    #expect(groups[0].lines.map(\.text) == ["Read(~/docs/**)", "Bash(ls /tmp)"])
    #expect(groups[1].lines.map(\.text) == ["Bash(git push:*)"])
    #expect(groups[2].lines.map(\.text) == ["Read(~/.ssh/**)"])
}

@Test func onlyAllowedJsonRulesCanBeRemoved() {
    let lines = AgentPermissions.groups(rules: rules).flatMap(\.lines)
    #expect(lines.filter(\.removable).map(\.text) == ["Bash(ls /tmp)"])
    let own = lines.first { $0.removable }!
    #expect(AgentPermissions.ruleId(own) == "a1b2")
}

@Test func emptyListsStillGetAGroup() {
    let groups = AgentPermissions.groups(rules: [])
    #expect(groups.count == 3)
    #expect(groups.allSatisfy { $0.lines.isEmpty })
}
