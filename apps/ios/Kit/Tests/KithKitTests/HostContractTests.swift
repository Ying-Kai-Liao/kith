import CryptoKit
import Foundation
import Testing
@testable import KithKit

// The host RPC contract (serve/contract/host/contract.json) as KithKit reads it: the generated file is the contract's,
// the member answers of today's host (fixtures/, written by the host's own test from real calls), an older (compat/)
// and a newer (unknown/) host all decode, both as the contract's types and as the app's own.

private let contractDir = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent() // HostContractTests.swift
    .deletingLastPathComponent() // KithKitTests
    .deletingLastPathComponent() // Tests
    .deletingLastPathComponent() // Kit
    .deletingLastPathComponent() // ios
    .deletingLastPathComponent() // apps
    .appending(path: "serve/contract/host")

private struct Fixture { var name: String; var method: String; var type: String; var body: Data }

private func fixtures(_ sub: String) throws -> [Fixture] {
    let dir = contractDir.appending(path: "fixtures/\(sub)")
    return try FileManager.default.contentsOfDirectory(atPath: dir.path).filter { $0.hasSuffix(".json") }.sorted().map { name in
        let raw = try JSONSerialization.jsonObject(with: Data(contentsOf: dir.appending(path: name))) as? [String: Any]
        let body = try JSONSerialization.data(withJSONObject: raw?["body"] ?? [:])
        return Fixture(name: sub + name, method: raw?["method"] as? String ?? "", type: raw?["type"] as? String ?? "", body: body)
    }
}

private func fixture(_ sub: String, _ name: String) throws -> Data {
    try #require(try fixtures(sub).first { $0.name == sub + name }).body
}

/// The app's own type for a method's answer, where it reads one; nil where it only waits for the call to finish.
private func decodeAsTheAppDoes(_ method: String, _ body: Data) throws -> Bool {
    struct BotRes: Decodable { var bot: Bot }
    struct EntryRes: Decodable { var entry: Entry? }
    struct BotsRes: Decodable { var bots: [BotGuest] }
    let d = JSONDecoder()
    switch HostContract.Method(rawValue: method) {
    case .hello: _ = try d.decode(Hello.self, from: body)
    case .startChat: _ = try d.decode(BotRes.self, from: body)
    case .history, .thread: _ = try d.decode(ThreadPage.self, from: body)
    case .react, .unsend, .send: _ = try d.decode(EntryRes.self, from: body)
    case .myBots: _ = try d.decode(BotsRes.self, from: body)
    case .linkCode: _ = try d.decode(LinkCode.self, from: body)
    case .kithInfo: _ = try d.decode(KithInfoResponse.self, from: body)
    case .projects: _ = try d.decode(ProjectList.self, from: body)
    default: return false
    }
    return true
}

@Test func hostContractGenIsMadeFromContractJSONAsItIsNow() throws {
    // Else: node serve/contract/gen.mjs
    let hash = SHA256.hash(data: try Data(contentsOf: contractDir.appending(path: "contract.json")))
    #expect(hash.map { String(format: "%02x", $0) }.joined() == HostContract.sha256)
}

@Test(arguments: ["", "compat/", "unknown/"])
func everyHostAnswerDecodes(_ sub: String) throws {
    let all = try fixtures(sub)
    #expect(!all.isEmpty)
    for f in all {
        #expect(HostContract.Method(rawValue: f.method) != nil, "\(f.name): no method \(f.method)")
        do {
            #expect(try HostContract.decode(f.type, f.body), "\(f.name): no such type")
            _ = try decodeAsTheAppDoes(f.method, f.body)
        } catch {
            Issue.record("\(f.name): \(error)")
        }
    }
}

@Test func theAppReadsTheHostsRealAnswersWithItsOwnTypes() throws {
    let read = try fixtures("").filter { try decodeAsTheAppDoes($0.method, $0.body) }.map(\.method)
    for m in ["hello", "startChat", "history", "thread", "react", "unsend", "myBots", "linkCode", "kithInfo", "projects"] {
        #expect(read.contains(m), "\(m): no fixture read as the app reads it")
    }
}

@Test func aNewerHostsWordsReadAsTheContractSays() throws {
    let d = JSONDecoder()
    let bot = try d.decode(HostContract.BotAnswer.self, from: fixture("unknown/", "start-chat-new-words.json")).bot
    #expect(bot.kind == .agent)
    #expect(bot.status == .idle)
    #expect(bot.permission == .unknown)
    let history = try d.decode(HostContract.Entries.self, from: fixture("unknown/", "history-new-kind.json"))
    #expect(history.entries.first?.kind == .unknown)
    let hello = try d.decode(HostContract.Hello.self, from: fixture("unknown/", "hello-new-words.json"))
    #expect(hello.device == .desktop)
    #expect(hello.grantedAgents?.first?.role == .guest)
    let guest = try #require(try d.decode(HostContract.BotGuests.self, from: fixture("unknown/", "my-bots-new-words.json")).bots.first)
    #expect(guest.provider == .other)
    #expect(guest.state == .unknown)
    #expect(try d.decode(HostContract.ThreadProjects.self, from: fixture("unknown/", "thread-projects-new-by.json")).by == .inferred)
    #expect(try d.decode(HostContract.Citation.self, from: fixture("unknown/", "citation-new-error.json")).error == .notShared)
}

@Test func anOlderHostsHelloHasNoGrantedAgents() throws {
    let hello = try JSONDecoder().decode(Hello.self, from: fixture("compat/", "hello-before-granted-agents.json"))
    #expect(hello.grantedAgents == nil)
}

@Test func everyContractMethodIsAMembersOrAnUnprovenDevices() {
    #expect(Set(HostContract.Method.allCases) == Set(HostContract.memberMethods + HostContract.unprovenMethods))
}
