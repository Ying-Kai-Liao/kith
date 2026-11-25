import Foundation
import Testing
@testable import KithKit
@testable import KithUI

private let link = InviteLink(computerId: "comp_1", inviteId: "inv_1", token: String(repeating: "a", count: 43))

private struct Failing: InvitePreviewProvider {
    struct Boom: Error {}
    func preview(for link: InviteLink) async throws -> InvitePreview { throw Boom() }
}

private func bot(_ json: String) throws -> Bot { try JSONDecoder().decode(Bot.self, from: Data(json.utf8)) }

@Test func mockPreviewIsTheSameForAnInvite() async throws {
    let a = try await MockInvitePreviewProvider().preview(for: link)
    let b = try await MockInvitePreviewProvider().preview(for: link)
    #expect(a == b)
    #expect(a.items.map(\.kind) == [.agent, .room])
}

@Test func itemKindsAreAgentAndRoomOnTheWire() throws {
    let item = InvitePreviewItem(kind: .room, id: "r", name: "R", suitableFor: "x")
    let json = String(decoding: try JSONEncoder().encode(item), as: UTF8.self)
    #expect(json.contains(#""kind":"room""#))
}

@MainActor
@Test func modelLoadsThePreview() async {
    let model = InvitePreviewModel(link: link, provider: MockInvitePreviewProvider())
    #expect(model.phase == .loading)
    await model.load()
    #expect(model.preview?.inviterName == "Kai")
}

@MainActor
@Test func aFailingProviderFallsBackToGeneric() async {
    let model = InvitePreviewModel(link: link, provider: Failing())
    await model.load()
    #expect(model.phase == .generic)
    #expect(model.preview == nil)
}

@Test func doneTargetPicksAMatchingBotElseNil() throws {
    let items = [
        InvitePreviewItem(kind: .agent, id: "finance", name: "Finance", suitableFor: ""),
        InvitePreviewItem(kind: .room, id: "family", name: "Family", suitableFor: ""),
    ]
    let byName = try bot(#"{"id":"b9","name":"family"}"#)
    let byId = try bot(#"{"id":"finance","name":"x"}"#)
    #expect(InvitePreviewModel.target(for: items, in: [byName])?.id == "b9")
    #expect(InvitePreviewModel.target(for: items, in: [byName, byId])?.id == "finance")
    #expect(InvitePreviewModel.target(for: items, in: [try bot(#"{"id":"z","name":"Other"}"#)]) == nil)
    #expect(InvitePreviewModel.target(for: [], in: [byName]) == nil)
}
