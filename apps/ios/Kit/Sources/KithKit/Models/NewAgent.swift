import Foundation

/// "New agent": every bot is a kith agent, made through the host's `newKithAgent` (`kith new --json`). Mirrors the
/// desktop's `shared/new-agent.ts`; the host and kith have the last word on every rule here.
public enum NewAgent {
    /// The engines the form offers; `""` is kith's own default.
    public static let engines: [(id: String, label: String)] = [("", "Default"), ("claude", "Claude"), ("codex", "Codex")]

    /// What the form holds.
    public struct Form: Equatable, Sendable {
        public var name = ""
        public var description = ""
        public var engine = ""
        public var model = ""

        public init(name: String = "", description: String = "", engine: String = "", model: String = "") {
            self.name = name
            self.description = description
            self.engine = engine
            self.model = model
        }

        /// "Make a kith agent like this": a flagged row's name, one-line description, engine and model. An engine kith
        /// can't run (a registry agent, a custom command) stays at kith's default.
        public init(like bot: Bot) {
            let engine = NewAgent.engines.contains { $0.id == bot.backend && !$0.id.isEmpty } ? bot.backend : ""
            let line = bot.description.split(whereSeparator: \.isNewline).first { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
            var about = (line.map(String.init) ?? "").trimmingCharacters(in: .whitespaces)
            while about.hasPrefix("-") { about.removeFirst() }
            self.init(name: NewAgent.suggestedName(bot.name),
                      description: about.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: ": ", with: " - "),
                      engine: engine, model: engine.isEmpty ? "" : (bot.model ?? ""))
        }

        /// The `newKithAgent` params: optional picks are left out when untouched.
        public var params: [String: String] {
            var p = ["name": name.trimmingCharacters(in: .whitespaces), "description": description.trimmingCharacters(in: .whitespaces)]
            if !engine.isEmpty { p["harness"] = engine }
            let m = model.trimmingCharacters(in: .whitespaces)
            if !m.isEmpty { p["model"] = m }
            return p
        }
    }

    /// A typed or existing name, made into the closest valid agent name to start from.
    public static func suggestedName(_ typed: String) -> String {
        var out = ""
        for ch in typed.lowercased() {
            if ch.isASCII, ch.isLetter || ch.isNumber { out.append(ch) } else if !out.hasSuffix("-") { out.append("-") }
        }
        while let first = out.first, !(first.isASCII && first.isLetter) { out.removeFirst() }
        while out.hasSuffix("-") { out.removeLast() }
        return String(out.prefix(31))
    }

    /// The name rules the host enforces: 2-31 chars, lowercase letters, digits and dashes, starting with a letter, not "kith".
    public static func nameError(_ name: String) -> String? {
        let n = name.trimmingCharacters(in: .whitespaces)
        if n.isEmpty { return "Name the agent." }
        if n.count < 2 || n.count > 31 { return "Use 2 to 31 characters." }
        if !(n.first.map { $0.isASCII && $0.isLowercase && $0.isLetter } ?? false) { return "Start with a lowercase letter." }
        if !n.allSatisfy({ $0.isASCII && (($0.isLetter && $0.isLowercase) || $0.isNumber || $0 == "-") }) {
            return "Use lowercase letters, numbers and dashes only."
        }
        if n == "kith" { return "That name is kept for Kith itself." }
        return nil
    }

    /// One line, no ": ", no leading dash.
    public static func descriptionError(_ description: String) -> String? {
        let d = description.trimmingCharacters(in: .whitespaces)
        if d.isEmpty { return "Say in a line what the agent does." }
        if d.contains(where: \.isNewline) { return "Keep it to one line." }
        if d.hasPrefix("-") { return "Don't start with a dash." }
        if d.contains(": ") { return "Leave out \": \" (a colon and a space)." }
        return nil
    }

    /// The host made the agent but a later step failed: its words start with "<agent> was made, but".
    public static func madeButFailed(_ message: String, name: String) -> Bool {
        message.hasPrefix("\(name) was made, but")
    }

    public static func engineNotRunHere(_ name: String) -> String {
        "\(name) was made. Its engine isn't one this app runs yet, so open it from Terminal with `kith \(name)`."
    }

    /// `newKithAgent`'s answer: the agent's name, and its bot once the host has it (nil while the roster lags or its engine isn't one the host runs).
    public struct Answer: Decodable, Sendable {
        public var agent: String
        public var botId: String?
        public var bot: Bot?

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            agent = try c.decodeIfPresent(String.self, forKey: .agent) ?? ""
            bot = try? c.decodeIfPresent(Bot.self, forKey: .bot)
            botId = bot?.id ?? (try? c.decodeIfPresent(String.self, forKey: .botId))
        }

        private enum CodingKeys: String, CodingKey { case agent, botId, bot }
    }
}
