import Foundation

/// The words a routine row shows: its schedule, when it last ran and what needs attention. The host already sends
/// `triggerDescriptions`; these come first from the triggers themselves so a cron reads the same everywhere, and the
/// host's words (or the raw expression) cover what can't be put into words here.
public enum RoutineSummary {
    /// "Every day at 9:00", "Weekdays at 18:30", "Every hour"; the raw text when it can't be put into words.
    public static func schedule(_ routine: Routine) -> String {
        var parts: [String] = []
        for (i, trigger) in routine.triggers.enumerated() {
            let described = i < routine.triggerDescriptions.count ? routine.triggerDescriptions[i] : nil
            if let words = words(trigger) {
                parts.append(words)
            } else if let described, !described.isEmpty {
                parts.append(described)
            } else if let raw = raw(trigger) {
                parts.append(raw)
            }
        }
        if parts.isEmpty { parts = routine.triggerDescriptions.filter { !$0.isEmpty } }
        return parts.joined(separator: " · ")
    }

    private static func raw(_ trigger: RoutineTrigger) -> String? {
        trigger.expression ?? (trigger.type == "webhook" ? "When a webhook fires" : nil)
    }

    private static func words(_ trigger: RoutineTrigger) -> String? {
        switch trigger.type {
        case "interval": return trigger.seconds.flatMap { every($0) }
        case "cron": return trigger.expression.flatMap { cron($0) }
        default: return nil
        }
    }

    private static func every(_ seconds: Int64) -> String? {
        guard seconds > 0 else { return nil }
        for (unit, name) in [(Int64(86_400), "day"), (3_600, "hour"), (60, "minute")] where seconds % unit == 0 {
            let n = seconds / unit
            return n == 1 ? "Every \(name)" : "Every \(n) \(name)s"
        }
        return nil
    }

    private static let weekdayNames = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]

    /// Five-field cron (minute hour day-of-month month day-of-week). Nil for anything beyond the common shapes.
    public static func cron(_ expression: String) -> String? {
        let f = expression.split(separator: " ", omittingEmptySubsequences: true).map(String.init)
        guard f.count == 5, f[2] == "*", f[3] == "*" else { return nil }
        let (minute, hour, dow) = (f[0], f[1], f[4])
        if let n = step(minute), hour == "*", dow == "*" { return n == 1 ? "Every minute" : "Every \(n) minutes" }
        guard let m = Int(minute), (0..<60).contains(m) else { return nil }
        if hour == "*" { return dow == "*" ? (m == 0 ? "Every hour" : "Every hour at :\(pad(m))") : nil }
        if let n = step(hour), m == 0, dow == "*" { return n == 1 ? "Every hour" : "Every \(n) hours" }
        guard let h = Int(hour), (0..<24).contains(h) else { return nil }
        let time = "\(h):\(pad(m))"
        switch dow {
        case "*": return "Every day at \(time)"
        case "1-5": return "Weekdays at \(time)"
        case "0,6", "6,0": return "Weekends at \(time)"
        default:
            let days = dow.split(separator: ",").compactMap { Int($0) }
            guard !days.isEmpty, days.count == dow.split(separator: ",").count, days.allSatisfy({ (0...7).contains($0) }) else {
                return nil
            }
            let names = days.map { weekdayNames[$0 % 7] }
            if names.count == 1 { return "Every \(names[0]) at \(time)" }
            return names.dropLast().joined(separator: ", ") + " and " + names[names.count - 1] + " at \(time)"
        }
    }

    /// `*/N` as N.
    private static func step(_ field: String) -> Int? {
        guard field.hasPrefix("*/"), let n = Int(field.dropFirst(2)), n > 0 else { return nil }
        return n
    }

    private static func pad(_ n: Int) -> String { n < 10 ? "0\(n)" : "\(n)" }

    /// The latest run of this routine, by start time.
    public static func lastRun(of routine: Routine, in runs: [RoutineRun]) -> RoutineRun? {
        runs.filter { $0.routineId == routine.id }.max { $0.createdAt < $1.createdAt }
    }

    /// "Last ran 2 hours ago", or "Never ran" when the host lists no run for it.
    public static func lastRan(_ run: RoutineRun?, now: Date = Date()) -> String {
        guard let run else { return "Never ran" }
        let at = Date(timeIntervalSince1970: Double(run.finishedAt ?? run.createdAt) / 1000)
        return "Last ran \(ago(now.timeIntervalSince(at)))"
    }

    static func ago(_ seconds: TimeInterval) -> String {
        let s = max(0, Int(seconds))
        if s < 60 { return "just now" }
        if s < 3_600 { return plural(s / 60, "minute") }
        if s < 86_400 { return plural(s / 3_600, "hour") }
        if s < 172_800 { return "yesterday" }
        return plural(s / 86_400, "day")
    }

    private static func plural(_ n: Int, _ unit: String) -> String { "\(n) \(unit)\(n == 1 ? "" : "s") ago" }

    /// The attention line: the reason the host gave, or that the last run failed. Nil when nothing needs attention.
    public static func attention(_ routine: Routine, lastRun: RoutineRun?) -> String? {
        if let error = routine.lastError, !error.isEmpty { return "Needs attention: \(error)" }
        if let lastRun, ["failed", "interrupted"].contains(lastRun.status) {
            let why = lastRun.detail.flatMap { $0.isEmpty ? nil : ": \($0)" } ?? ""
            return "Needs attention: last run \(lastRun.status)\(why)"
        }
        return nil
    }
}
