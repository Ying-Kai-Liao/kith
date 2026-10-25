import KithKit
import Foundation

/// A realtime model is an operator in front of the bot, not a replacement for it: the bot does the
/// work, the operator talks. Its tools run here against `BotStore`; only `send_to_bot` messages and
/// the bot's replies become chat entries.
@MainActor
struct CallOperator {
    let model: BotStore
    let botId: String

    var instructions: String {
        let bot = model.bots[botId]
        let name = bot?.name ?? "the bot"
        let about = bot.map { $0.description.isEmpty ? "" : " (\($0.description))" } ?? ""
        let who = if let bot, bot.isGroup {
            "the group chat \(name)\(about) on the user's computer, where the bots "
                + model.members(of: bot).map(\.name).joined(separator: ", ")
                + " answer in turn (a message that @-mentions one of them asks just that bot). "
                + "The bots do all the actual work; you relay"
        } else {
            "\(name)\(about), a coding agent running on the user's computer. \(name) does all the actual work; you relay"
        }
        return """
        You are the voice on a phone call between the user and \(who).
        - When the user asks for something to be done, changed, checked or answered about their \
        code or computer, call send_to_bot with their request in their own words (keep details, \
        names and code terms exactly). Don't do the work or invent results yourself.
        - Use bot_status and recent_messages for "what are you doing?" or "what did it say?".
        - Call answer_approval only after the user has clearly said which option to pick.
        - Messages starting with [\(name) replied] or [Notice] come from the computer: tell the \
        user what they say, shortened for speech (no code blocks, no long lists, no URLs).
        - Reply in the user's language. Keep spoken answers short and natural.
        """
    }

    /// Function declarations in JSON Schema form (both providers take the same shape).
    static let tools: [(name: String, description: String, parameters: [String: any Sendable])] = [
        ("send_to_bot", "Send a message to the bot on the computer, exactly as if the user typed it. Its reply arrives later.",
         ["type": "object", "properties": ["text": ["type": "string", "description": "The message"]], "required": ["text"]]),
        ("bot_status", "Whether the bot is idle or working, what it is doing, and any approval it is waiting for.",
         ["type": "object", "properties": [String: any Sendable]()]),
        ("recent_messages", "The latest messages in the chat with the bot, oldest first.",
         ["type": "object", "properties": ["count": ["type": "integer", "description": "How many, 1–20"]]]),
        ("answer_approval", "Answer the bot's pending approval request with one of its options.",
         ["type": "object", "properties": ["option": ["type": "string", "description": "The option's name"]], "required": ["option"]]),
    ]

    /// Runs one tool call; the result is a JSON string.
    func call(_ name: String, arguments: [String: Any]) -> String {
        let result: [String: Any] = switch name {
        case "send_to_bot": send(arguments["text"] as? String ?? "")
        case "bot_status": status()
        case "recent_messages": recent(arguments["count"] as? Int ?? 6)
        case "answer_approval": answer(arguments["option"] as? String ?? "")
        default: ["error": "Unknown tool \(name)."]
        }
        let data = (try? JSONSerialization.data(withJSONObject: result)) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    /// How a reply is handed to the operator.
    func reply(_ text: String) -> String {
        "[\(model.bots[botId]?.name ?? "Bot") replied] \(SpokenText.from(text))"
    }

    func notice(_ text: String) -> String { "[Notice] \(text)" }

    private func send(_ text: String) -> [String: Any] {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return ["error": "Nothing to send."] }
        model.sendSpoken(text, to: botId)
        return ["sent": true, "note": "The bot's reply will arrive as a separate message."]
    }

    private func status() -> [String: Any] {
        guard let bot = model.bots[botId] else { return ["error": "The bot isn't available."] }
        var out: [String: Any] = ["status": bot.status, "activity": bot.activity]
        if let card = pendingApproval {
            out["approval"] = [
                "request": card.data.title ?? card.data.command ?? "",
                "options": (card.data.options ?? []).map(\.name),
            ]
        }
        return out
    }

    private func recent(_ count: Int) -> [String: Any] {
        let name = model.bots[botId]?.name ?? "bot"
        let messages = Tasks.chatAndTasks(model.allEntries(botId)).filter(\.isChat).filter { !$0.data.isSecretCard }.suffix(min(max(count, 1), 20)).map { e -> [String: String] in
            let from = switch e.kind {
            case "user": "user"
            case "agent": e.data.author.flatMap { model.bots[$0]?.name } ?? name
            case "permission": "approval request"
            default: "notice"
            }
            return ["from": from, "text": SpokenText.from(e.data.text ?? e.data.title ?? "")]
        }
        return ["messages": Array(messages)]
    }

    private func answer(_ option: String) -> [String: Any] {
        guard let card = pendingApproval else { return ["error": "Nothing is waiting for approval."] }
        let options = card.data.options ?? []
        let wanted = option.lowercased()
        guard let pick = options.first(where: { $0.name.lowercased() == wanted || $0.optionId == option })
            ?? options.first(where: { $0.name.lowercased().contains(wanted) || wanted.contains($0.name.lowercased()) })
        else {
            return ["error": "No such option.", "options": options.map(\.name)]
        }
        model.respond(card, option: pick.optionId)
        return ["answered": pick.name]
    }

    private var pendingApproval: Entry? {
        // A key card is never read aloud or answered by voice: the key is typed on the card.
        Tasks.chatAndTasks(model.allEntries(botId)).last { $0.kind == "permission" && $0.data.status == "pending" && !$0.data.isSecretCard }
    }
}
