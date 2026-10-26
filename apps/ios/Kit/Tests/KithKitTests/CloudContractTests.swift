import CryptoKit
import Foundation
import Testing
@testable import KithKit

// The cloud contract (serve/contract/cloud/contract.json) as KithKit reads it: the generated file is the contract's,
// and the answers of today's, an older (compat/) and a newer (unknown/) cloud all decode.

private let contractDir = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent() // CloudContractTests.swift
    .deletingLastPathComponent() // KithKitTests
    .deletingLastPathComponent() // Tests
    .deletingLastPathComponent() // Kit
    .deletingLastPathComponent() // ios
    .deletingLastPathComponent() // apps
    .appending(path: "serve/contract/cloud")

private struct Fixture { var name: String; var type: String; var body: Data }

private func fixtures(_ sub: String) throws -> [Fixture] {
    let dir = contractDir.appending(path: "fixtures/\(sub)")
    return try FileManager.default.contentsOfDirectory(atPath: dir.path).filter { $0.hasSuffix(".json") }.sorted().map { name in
        let raw = try JSONSerialization.jsonObject(with: Data(contentsOf: dir.appending(path: name))) as? [String: Any]
        let body = try JSONSerialization.data(withJSONObject: raw?["body"] ?? [:])
        return Fixture(name: sub + name, type: raw?["type"] as? String ?? "", body: body)
    }
}

@Test func cloudContractGenIsMadeFromContractJSONAsItIsNow() throws {
    // Else: node serve/contract/gen.mjs
    let hash = SHA256.hash(data: try Data(contentsOf: contractDir.appending(path: "contract.json")))
    #expect(hash.map { String(format: "%02x", $0) }.joined() == CloudContract.sha256)
}

@Test(arguments: ["", "compat/", "unknown/"])
func everyCloudAnswerDecodes(_ sub: String) throws {
    let all = try fixtures(sub)
    #expect(!all.isEmpty)
    for f in all {
        do {
            #expect(try CloudContract.decode(f.type, f.body), "\(f.name): no such type")
        } catch {
            Issue.record("\(f.name): \(error)")
        }
    }
}

@Test func aNewerCloudsWordsReadAsTheContractSays() throws {
    let list = try #require(try fixtures("unknown/").first { $0.type == "ComputerList" })
    let computer = try #require(try JSONDecoder().decode(ComputerList.self, from: list.body).computers.first)
    // ComputerRole and ComputerAccess name a value for a word they don't know: the least.
    #expect(computer.role == .member)
    #expect(computer.isMember)
    #expect(computer.access == .noAccess)
    // AccessState's unknown is its own state.
    let state = try #require(try fixtures("unknown/").first { $0.type == "AccessStatus" })
    #expect(try JSONDecoder().decode(AccessStatus.self, from: state.body).status == .unknown)
    // #328: a computer's access is never "approved"; it reads as none, not as granted.
    let approved = try JSONDecoder().decode(CloudComputer.self, from: Data(#"{"computerId":"c","name":"n","signKey":"k","access":"approved"}"#.utf8))
    #expect(approved.access == .noAccess)
}

@Test func anOlderCloudsComputersAreTheOwners() throws {
    let list = try #require(try fixtures("compat/").first { $0.type == "ComputerList" })
    let computers = try JSONDecoder().decode(ComputerList.self, from: list.body).computers
    #expect(!computers.isEmpty)
    #expect(computers.allSatisfy { $0.role == nil && !$0.isMember })
}

@Test func errorCodesHaveTheContractsStatus() {
    #expect(CloudErrorCode(code: "requestExpired") == .requestExpired)
    #expect(CloudErrorCode.requestExpired.status == 410)
    #expect(CloudErrorCode(code: "somethingNew") == .unknown)
    #expect(CloudError(status: 410, code: "requestExpired").errorDescription == "The request expired. Ask again.")
}

@Test func pathsAreTheContracts() {
    #expect(CloudPath.relayDevice("c1") == "v1/relay/device/c1?v=1")
    #expect(CloudPath.acceptInvite("inv_1") == "v1/invites/inv_1/accept")
}
