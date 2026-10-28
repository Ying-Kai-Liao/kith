import Foundation

/// A Schedule card's buttons: kith-sched for a Kith job (`kithSchedule`), the routines API for a routine.
public enum ScheduleOp: String, Sendable {
    case pause, resume, run, remove
}

/// Why a button's call failed, in kith's words.
public struct ScheduleError: LocalizedError {
    public var message: String
    public init(message: String) { self.message = message }
    public var errorDescription: String? { message }
}

/// The card's foot (DESIGN.md "Schedule"): the owner's buttons, the in-card remove confirm, or a line saying who can.
/// Desktop: `scheduleFoot` in `schedule-card.ts`.
public enum ScheduleFoot: Hashable, Sendable {
    public struct Toggle: Hashable, Sendable {
        public var op: ScheduleOp
        public var label: String
        /// Resume is the ink button; Pause is white.
        public var ink: Bool
    }

    case note(String)
    case confirm(question: String, removing: Bool)
    case buttons(toggle: Toggle, run: String, runDisabled: Bool, busy: Bool)

    /// `busy` is the button whose call is on its way; Run now reads "Running…" until the run ends. `ownerName` comes
    /// from the host's "Waiting for Kai" line; without it, "The owner".
    public init(_ view: ScheduleCardModel, owner: Bool, ownerName: String?, confirming: Bool, busy: ScheduleOp?) {
        if !owner {
            self = .note("\(ownerName.flatMap { $0.isEmpty ? nil : $0 } ?? "The owner") can pause or run it.")
        } else if confirming {
            self = .confirm(question: "Remove \(view.name)? It won't run again, and its task goes with it.", removing: busy == .remove)
        } else {
            let running = view.last.running || busy == .run
            self = .buttons(
                toggle: view.paused ? Toggle(op: .resume, label: "Resume", ink: true) : Toggle(op: .pause, label: "Pause", ink: false),
                run: running ? "Running…" : "Run now",
                runDisabled: running || busy != nil,
                busy: busy != nil
            )
        }
    }

    /// The owner's name in the host's activity line for a guest ("Waiting for Kai"), else nil.
    public static func ownerName(from activity: String?) -> String? {
        guard let activity, activity.hasPrefix("Waiting for ") else { return nil }
        let name = activity.dropFirst("Waiting for ".count).trimmingCharacters(in: .whitespaces)
        return name.isEmpty ? nil : name
    }
}
