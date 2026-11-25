import KithKit
import Foundation

/// Who a share goes to: someone already on the computer, or a new person (named; they get an invite link).
public enum ShareRecipient: Hashable, Sendable {
    case existing(String)
    case new(name: String)
}

/// What a finished share left behind.
public enum ShareOutcome: Sendable {
    /// A new person: the link to pass on.
    case invite(CreatedInvite)
    /// An existing person: it is done.
    case updated(personId: String)
}

/// What the Sharing screens ask of a computer. `HostSharingService` answers it from the host, `MockSharingService`
/// from memory (previews, the demo, tests). Writes return false/nil on failure and leave the reason in `error`;
/// every write reloads, so two devices editing see each other.
@MainActor
public protocol SharingService: AnyObject {
    /// Everyone the owner shared with, removed people last.
    var persons: [SharingPerson] { get }
    /// What can be shared: the owner's agents and rooms.
    var agents: [String] { get }
    var rooms: [String] { get }
    var loaded: Bool { get }
    /// The computer's host can't do sharing (older than invites): the screens stay away.
    var unsupported: Bool { get }
    /// A write is in flight: Share and the edit buttons wait, so a double tap makes one invite.
    var busy: Bool { get }
    var error: String? { get }
    /// Who was just removed, so the screen can offer Undo until the owner does something else.
    var removed: SharingPerson? { get }

    func reload() async
    func share(_ what: [ShareTarget], with recipient: ShareRecipient, level: ShareLevel) async -> ShareOutcome?
    func setLevel(_ level: ShareLevel, of agent: String, for person: String) async -> Bool
    func unshare(_ what: ShareTarget, from person: String) async -> Bool
    /// Ends all their access (their chats and memory stay); `restore` undoes it.
    func stopSharing(person: String) async -> Bool
    func restore(person: String) async
    func cancelInvite(_ inviteId: String) async
    func dismissRemoved()
    /// The invite's link, while this device knows it (only right after it was made).
    func link(for inviteId: String) -> String?
}

public extension SharingService {
    func person(_ id: String) -> SharingPerson? { persons.first { $0.id == id } }

    /// People who hold the agent (by their own share or everyone's), with the share that says how.
    func holders(of agent: String) -> [(person: SharingPerson, share: Share)] {
        persons.compactMap { p in p.share(.agent(agent)).map { (p, $0) } }.filter { $0.person.status != .removed }
    }

    /// "Shared with 3 people", for the Settings row.
    var summary: String {
        let count = persons.filter { $0.status != .removed }.count
        return count == 0 ? "Share an agent" : count == 1 ? "Shared with 1 person" : "Shared with \(count) people"
    }
}
