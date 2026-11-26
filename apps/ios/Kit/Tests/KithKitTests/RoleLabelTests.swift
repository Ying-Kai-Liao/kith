import Testing
@testable import KithKit

@Test func roleLabelsWordThePresets() {
    #expect(InviteRole.guest.levelTitle == "Can ask")
    #expect(InviteRole.member.levelTitle == "Can work")
    #expect(InviteRole.guest.levelMeaning == "Answers, and asks you before changing anything")
    #expect(InviteRole.member.levelMeaning == "Can make changes")
}

@Test func personStateFollowsRevokedAndJoined() {
    #expect(PersonState(HostPerson(personId: "sam", name: "Sam", joined: true)) == .active)
    #expect(PersonState(HostPerson(personId: "sam", name: "Sam", joined: false)) == .invited)
    #expect(PersonState(HostPerson(personId: "sam", name: "Sam", joined: true, revoked: true)) == .paused)
    // Paused wins over not-joined: a revoked invitee can still be resumed.
    #expect(PersonState(HostPerson(personId: "jo", name: "Jo", joined: false, revoked: true)) == .paused)
}
