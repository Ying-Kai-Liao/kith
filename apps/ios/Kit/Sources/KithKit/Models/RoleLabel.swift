import Foundation

/// How the app words the two stored presets (`guest`, `member`) to the owner. Stored names stay as they are.
public extension InviteRole {
    /// "Can ask" (guest) or "Can work" (member).
    var levelTitle: String { self == .member ? "Can work" : "Can ask" }

    /// One line on what the level means. A guest is the mode default, not read-only.
    var levelMeaning: String {
        self == .member ? "Can make changes" : "Answers, and asks you before changing anything"
    }
}

/// Where a person stands on the owner's computer, for the person page.
public enum PersonState: Equatable, Sendable {
    /// Holds access now.
    case active
    /// The owner suspended them (`revokePerson`); Resume undoes it.
    case paused
    /// Invited, has not accepted yet: nothing to pause.
    case invited

    public init(_ person: HostPerson) {
        if person.revoked {
            self = .paused
        } else if !person.joined {
            self = .invited
        } else {
            self = .active
        }
    }
}
