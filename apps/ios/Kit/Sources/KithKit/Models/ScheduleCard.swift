import Foundation

/// The Schedule card (DESIGN.md "Schedule"): a Kith agent's kith-sched job (`ref {agent, id}`) or an outside bot's
/// routine (`ref {bot, routine}`), drawn from the live job when the app can read it and from the entry's snapshot when
/// it can't or the job is gone. Every word is host state or the card's fixed text. Desktop: `schedule-card.ts`.
public struct ScheduleLive: Hashable, Sendable {
    public struct Last: Hashable, Sendable {
        public var running: Bool
        public var at: Date
        public init(running: Bool, at: Date) {
            self.running = running
            self.at = at
        }
    }

    public var paused: Bool
    public var nextRun: Date?
    public var last: Last?
    /// A routine's last run failed (Kith jobs carry no success word).
    public var failed: Bool
    public var error: String?

    public init(paused: Bool, nextRun: Date?, last: Last?, failed: Bool = false, error: String? = nil) {
        self.paused = paused
        self.nextRun = nextRun
        self.last = last
        self.failed = failed
        self.error = error
    }

    /// A Kith job's live state from `kithInfo` schedules; nil when kith answered without it (removed).
    public static func kith(_ schedules: [KithSchedule], id: String) -> ScheduleLive? {
        guard let job = schedules.first(where: { $0.id == id }) else { return nil }
        return ScheduleLive(paused: job.paused, nextRun: job.nextRun?.date, last: job.lastRun.map { Last(running: job.isRunning, at: $0.date) })
    }

    /// A routine's live state from the `routines` listing (runs newest first); nil when it is gone.
    public static func routine(_ listing: RoutineListing, id: String) -> ScheduleLive? {
        guard let routine = listing.routines.first(where: { $0.id == id }) else { return nil }
        let run = listing.runs.first { $0.routineId == id }
        let running = run?.isActive == true
        let ms = { (v: Int64) in Date(timeIntervalSince1970: Double(v) / 1000) }
        return ScheduleLive(
            paused: !routine.enabled,
            nextRun: routine.nextRunAt.map(ms),
            last: run.map { Last(running: running, at: ms(running ? $0.createdAt : ($0.finishedAt ?? $0.createdAt))) },
            failed: run.map { ["failed", "interrupted"].contains($0.status) } ?? false,
            error: routine.lastError
        )
    }
}

public struct ScheduleCardModel: Hashable, Sendable {
    public struct Fold: Hashable, Sendable {
        public var text: String
        /// When, on the right: the entry's time or day.
        public var trailing: String
        /// A pill after its day opens in place; a changed or removed card stays a line.
        public var opens: Bool
        public var done: Bool

        public init(text: String, trailing: String, opens: Bool, done: Bool) {
            self.text = text
            self.trailing = trailing
            self.opens = opens
            self.done = done
        }
    }

    public struct LastRun: Hashable, Sendable {
        public var text: String
        public var running: Bool
        public var failed: Bool
        public var error: String?
    }

    public enum Kind: Sendable { case job, routine }

    public var kind: Kind
    public var name: String
    /// "Added · 9:12", "Updated · Tue"; nil when paused (the capsule shows instead).
    public var op: String?
    public var paused: Bool
    public var every: String
    public var next: String
    public var last: LastRun
    /// Whom a Kith job runs as: "you" or a person's name. Nil for a routine (it runs as its bot).
    public var runsAs: String?
    public var task: String
    public var fold: Fold?

    private static let ops = ["added": "Added", "updated": "Updated", "resumed": "Resumed", "paused": "Paused", "created": "Created", "removed": "Removed"]

    /// The card as it reads now. `live` is the job as the app read it (`.none`: not read, so the snapshot shows;
    /// `.some(nil)`: read and gone). `viewerIsPerson` is whether the viewer is the person the job runs as;
    /// `personName` names anyone else.
    public init(card: RichCard, postedAt: Date, now: Date = .now, live: ScheduleLive?? = .none, viewerIsPerson: Bool,
                personName: String?, opened: Bool = false, calendar: Calendar = .current) {
        let s = card.snapshot ?? [:]
        let ref = card.ref ?? [:]
        kind = CardWords.string(ref["routine"]) != nil ? .routine : .job
        name = CardWords.string(s["name"]) ?? "Schedule"
        let read: ScheduleLive? = live ?? nil
        paused = read?.paused ?? (s["paused"] == .bool(true))
        every = CardWords.string(s["every"]) ?? CardWords.string(s["cron"]) ?? ""
        let nextAt = read.map(\.nextRun) ?? CardWords.date(s["nextRun"])
        let last = read.map(\.last) ?? CardWords.date(s["lastRun"]).map { ScheduleLive.Last(running: s["lastStatus"] == .string("running"), at: $0) }
        let failed = read?.failed == true && last?.running != true
        runsAs = kind == .routine ? nil : viewerIsPerson ? "you" : (personName ?? "you")
        let op = CardWords.string(s["op"])
        let trailing = CardWords.posted(postedAt, now: now, calendar: calendar)

        let line = { (text: String, done: Bool) in Fold(text: text, trailing: trailing, opens: false, done: done) }
        if s["folded"] == .string("changed") {
            fold = line("\(name) · changed below", false)
        } else if s["folded"] == .string("removed") {
            fold = line("\(name) · removed below", false)
        } else if op == "removed" || (live != nil && read == nil) {
            fold = line("\(name) · removed\(runsAs == "you" ? " by you" : "")", true)
        } else if CardWords.days(postedAt, from: now, calendar: calendar) < 0, !opened {
            let next: String? = paused ? "paused" : nextAt.map {
                "next \(CardWords.days($0, from: now, calendar: calendar) == 0 ? CardWords.clock($0, calendar: calendar) : CardWords.when($0, now: now, calendar: calendar))"
            }
            let parts = [name, CardWords.quietEvery(every), next ?? ""].filter { !$0.isEmpty }
            fold = Fold(text: parts.joined(separator: " · "), trailing: trailing, opens: true, done: false)
        } else {
            fold = nil
        }

        self.op = paused ? nil : "\(Self.ops[op ?? ""] ?? "Added") · \(trailing)"
        next = paused ? "Paused" : nextAt.map { CardWords.when($0, now: now, calendar: calendar) } ?? "—"
        self.last = LastRun(
            text: last.map { $0.running ? "Running since \(CardWords.clock($0.at, calendar: calendar))" : CardWords.when($0.at, now: now, calendar: calendar) } ?? "Not run yet",
            running: last?.running == true,
            failed: failed,
            error: failed ? read?.error.flatMap { CardWords.string(.string($0)) } : nil
        )
        task = CardWords.shortPaths(CardWords.string(s["task"]) ?? "")
    }
}

/// The words the Schedule and News cards share: day words, times, quiet schedule words and short paths.
public enum CardWords {
    static func string(_ v: RichCard.JSON?) -> String? {
        guard case let .string(s)? = v, !s.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return s
    }

    static func date(_ v: RichCard.JSON?) -> Date? {
        switch v {
        case let .int(ms)?: Date(timeIntervalSince1970: Double(ms) / 1000)
        case let .double(ms)?: Date(timeIntervalSince1970: ms / 1000)
        case let .string(s)?: KithDate.parse(s)
        default: nil
        }
    }

    public static func clock(_ date: Date, calendar: Calendar = .current) -> String {
        let c = calendar.dateComponents([.hour, .minute], from: date)
        return "\(c.hour ?? 0):\(String(format: "%02d", c.minute ?? 0))"
    }

    /// Whole calendar days from `now` to `date` (negative in the past).
    public static func days(_ date: Date, from now: Date, calendar: Calendar = .current) -> Int {
        calendar.dateComponents([.day], from: calendar.startOfDay(for: now), to: calendar.startOfDay(for: date)).day ?? 0
    }

    static let shortDays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
    static let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]

    /// "Today", "Tomorrow", "Yesterday", a weekday within six days, else "31 Oct".
    public static func day(_ date: Date, now: Date, calendar: Calendar = .current) -> String {
        let d = days(date, from: now, calendar: calendar)
        switch d {
        case 0: return "Today"
        case 1: return "Tomorrow"
        case -1: return "Yesterday"
        case -6...6: return shortDays[calendar.component(.weekday, from: date) - 1]
        default: return "\(calendar.component(.day, from: date)) \(months[calendar.component(.month, from: date) - 1])"
        }
    }

    /// A time on a row: "Today 7:43", "Fri 7:30", "31 Oct 7:43".
    public static func when(_ date: Date, now: Date, calendar: Calendar = .current) -> String {
        "\(day(date, now: now, calendar: calendar)) \(clock(date, calendar: calendar))"
    }

    /// When an entry was posted, for a header or a pill: the time on its own day, else the day ("Tue", "31 Oct").
    public static func posted(_ date: Date, now: Date, calendar: Calendar = .current) -> String {
        days(date, from: now, calendar: calendar) == 0 ? clock(date, calendar: calendar) : day(date, now: now, calendar: calendar)
    }

    /// The schedule in a pill's quiet words: "Every day at 7:43" is "every day 7:43".
    public static func quietEvery(_ every: String) -> String {
        guard let first = every.first else { return "" }
        let lowered = first.lowercased() + every.dropFirst()
        guard let at = lowered.range(of: #" at (?=\d)"#, options: .regularExpression) else { return lowered }
        return lowered.replacingCharacters(in: at, with: " ")
    }

    /// Paths in a task shortened to their last two folders: "…/daily-news/SKILL.md".
    public static func shortPaths(_ task: String) -> String {
        let pattern = #"(?:~|/)[^\s'"`]*/([^\s/'"`]+/[^\s/'"`]+/[^\s/'"`]+)"#
        guard let regex = try? NSRegularExpression(pattern: pattern) else { return task }
        let source = task as NSString
        let out = NSMutableString(string: task)
        for match in regex.matches(in: task, range: NSRange(location: 0, length: source.length)).reversed()
        where match.range.length > match.range(at: 1).length + 1 {
            out.replaceCharacters(in: match.range, with: "…/" + source.substring(with: match.range(at: 1)))
        }
        return out as String
    }
}
