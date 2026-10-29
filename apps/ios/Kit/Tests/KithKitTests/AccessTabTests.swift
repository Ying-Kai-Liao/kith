import Foundation
import Testing
@testable import KithKit

@Test func theAccessTabKeepsTheKeysThisAgentHolds() {
    let keys = [
        KeyRecord(profile: "notion/sam", service: "notion", account: "sam", agents: ["coder", "designer"]),
        KeyRecord(profile: "github/work", service: "github", account: "work", agents: ["coder"]),
        KeyRecord(profile: "slack/team", service: "slack", account: "team", agents: ["designer"]),
    ]
    let mine = AccessTab.keys(keys, for: "coder")
    #expect(mine.map(\.profile) == ["github/work", "notion/sam"])
    #expect(AccessTab.accountTitle(mine[0]) == "github · work")
    #expect(AccessTab.keys(keys, for: "nobody").isEmpty)
}

@Test func aKeyDetailSaysKeychainAndWhenItWasLastUsed() {
    let used = KeyRecord(profile: "a/b", service: "a", account: "b", lastUsed: "2026-10-25T08:12:00Z")
    let never = KeyRecord(profile: "a/c", service: "a", account: "c")
    #expect(AccessTab.keyDetail(used, ago: { _, _ in "3 h ago" }) == "Keychain · last used 3 h ago")
    #expect(AccessTab.keyDetail(never, ago: { _, _ in "x" }) == "Keychain · not used yet")
}

@Test func grantsGroupByPersonInTheHostsOrder() {
    let grants = [
        AccessGrant(key: "1", kind: "read", person: "sam", name: "Sam", target: "~/docs"),
        AccessGrant(key: "2", kind: "domain", person: "jo", target: "pypi.org"),
        AccessGrant(key: "3", kind: "service", person: "sam", name: "Sam", target: "notion/sam"),
        AccessGrant(key: "4", kind: "read", person: "jo", name: "Jo", target: "~/x"),
        AccessGrant(key: "5", kind: "domain", person: "*", target: "example.com"),
    ]
    let people = AccessTab.people(grants)
    #expect(people.map(\.id) == ["sam", "jo", "*"])
    #expect(people.map(\.name) == ["Sam", "Jo", "Everyone"])
    #expect(people[0].grants.map(\.key) == ["1", "3"])
    #expect(people[1].grants.map(\.key) == ["2", "4"])
    #expect(AccessTab.people([]).isEmpty)
}

@Test func aRefusedOrMissingMethodHidesTheSection() {
    #expect(AccessTab.isUnavailable(HostError.http(403, "Not allowed")))
    #expect(AccessTab.isUnavailable(HostError.http(404, "Not found")))
    #expect(AccessTab.isUnavailable(HostError.http(400, "Unknown method")))
    #expect(!AccessTab.isUnavailable(HostError.http(500, "boom")))
    #expect(!AccessTab.isUnavailable(HostError.unreachable))
}
