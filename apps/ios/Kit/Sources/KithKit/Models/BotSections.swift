import Foundation

/// Which of Kith's own cards an hq bot's page still shows. The kith sections carry the same facts,
/// read from the agent's package, so each fact shows once and kith's copy wins. A card goes only once
/// kith has the fact on screen: while the details load, fail or lack it, every card stays, and a plain
/// bot (no `hqAgent`) always shows all of them. Mirrors the desktop's `botSections`.
public struct BotSections: Equatable, Sendable {
    /// "Standing instructions": kith's About shows the agent's role.
    public var instructions = true
    /// The agent (backend) picker: kith's Model & permissions shows the harness.
    public var backendPicker = true
    /// The harness's own notes tab in Memory. For an hq bot it is Claude Code's MEMORY.md of the bot's working
    /// folder (the host's `projectMemory`), not the agent's memory: kith's "Agent memory" lists the real files.
    /// Gone for every hq bot, loaded or not.
    public var harnessMemoryTab = true
    /// Kith's own permission, model, effort and Fast mode controls. hq is the one source for an hq bot's settings
    /// (the host ignores its stored copy), so they go and the `HqSetting` rows take their place. A host that predates
    /// `kithInfo` can't serve those, so the controls stay there.
    public var kithSettings = true

    public static let all = BotSections()

    public init() {}

    public init(hqAgent: String?, info: KithInfo?, unsupportedHost: Bool = false) {
        guard let hqAgent, !hqAgent.isEmpty else { return }
        harnessMemoryTab = false
        kithSettings = unsupportedHost
        guard let info else { return }
        instructions = (info.role == nil && (info.description ?? "").isEmpty) || info.errors["role"] != nil
        backendPicker = (info.harness ?? "").isEmpty
    }
}

/// One of an hq bot's settings as the apps show it, read from hq. Mirrors the desktop's `hqSettings`.
public struct HqSetting: Equatable, Sendable {
    public enum Key: String, Sendable { case permission, callModel, harness, model, effort, fastMode }
    public var key: Key
    public var label: String
    /// What hq reports, or plain words when it has nothing.
    public var value: String
    /// Written through to hq (`kith <agent> set …`). Fast mode has no kith setting, so it is no row at all.
    public var editable: Bool
    public var hint: String

    /// Effort levels `kith <agent> set effort` takes, plus `default` (forget the choice). Mirrors the desktop's `EFFORT_LEVELS`.
    public static let effortLevels: [(id: String, label: String)] = [
        ("default", "Agent's default"), ("low", "Low"), ("medium", "Medium"), ("high", "High"), ("xhigh", "Extra high"), ("max", "Max"),
    ]

    /// The harnesses the picker offers for a kith agent; the current one is added when it is another.
    public static let harnesses: [(id: String, label: String)] = [("claude", "Claude"), ("codex", "Codex")]

    /// Claude Code's own permission modes, in the order the picker lists them. Kith passes the pick to the agent unchanged.
    public static let permissionModes: [(id: String, label: String)] = [
        ("default", "Default"), ("acceptEdits", "Accept edits"), ("plan", "Plan only"),
        ("auto", "Auto"), ("bypassPermissions", "Bypass permissions"),
    ]

    /// A stored mode as the picker knows it: the old `ask` and `manual` are `default`, a stored `dontAsk` is `auto`.
    public static func permissionMode(_ mode: String) -> String {
        mode == "ask" || mode == "manual" ? "default" : mode == "dontAsk" ? "auto" : mode
    }

    /// One line under each mode in the picker's menu; the row itself keeps showing just the label.
    public static func permissionDetail(_ mode: String) -> String? {
        switch permissionMode(mode) {
        case "default": "Asks the first time each tool is used"
        case "acceptEdits": "File edits are accepted without asking; other actions ask"
        case "plan": "Read-only planning; changes nothing"
        case "auto": "A classifier approves safe actions; risky ones ask or are blocked"
        case "bypassPermissions": "No permission checks"
        default: nil
        }
    }

    public static func permissionLabel(_ mode: String) -> String {
        permissionModes.first { $0.id == permissionMode(mode) }?.label ?? mode
    }

    private static let callModelNames: [(id: String, label: String)] = [
        ("sonnet", "Sonnet"), ("haiku", "Haiku"), ("opus", "Opus"), ("own", "Agent's own model"),
    ]

    /// What a voice call can run on (`kith <agent> set call-model`). `default` follows this Mac's global call model
    /// (`defaultIs`, sonnet unless changed); `own` is the agent's own model.
    public static func callModels(defaultIs: String? = nil) -> [(id: String, label: String)] {
        let name = defaultIs ?? "sonnet"
        let shown = callModelNames.first { $0.id == name }?.label.lowercased() ?? name
        return [(id: "default", label: "Default (\(shown))")] + callModelNames
    }

    public static func callModelLabel(_ id: String, defaultIs: String? = nil) -> String {
        callModels(defaultIs: defaultIs).first { $0.id == id }?.label ?? id
    }

    /// `info` is nil while kith's answer loads or failed: nothing is editable then.
    public static func rows(_ info: KithInfo?) -> [HqSetting] {
        return [
            HqSetting(key: .permission, label: "Permissions", value: info.map { permissionMode($0.permissionMode ?? "auto") } ?? "…",
                      editable: info != nil,
                      hint: "Kept by Kith on this computer. Every chat of this agent uses it from its next turn."),
            HqSetting(key: .callModel, label: "Call model", value: info.map { $0.callModel ?? "default" } ?? "…",
                      editable: info != nil,
                      hint: "The model that answers when you talk to this agent in a voice call. Typed messages use the agent's own model."),
            HqSetting(key: .harness, label: "Harness", value: info.map { $0.harness ?? "claude" } ?? "…", editable: info != nil,
                      hint: "The program that runs this agent on this computer. Switching drops a model the new harness can't run."),
            HqSetting(key: .model, label: "Model", value: info.map { $0.model ?? "default" } ?? "…", editable: info != nil,
                      hint: "Kept by Kith for this agent. Every chat of this agent uses it from its next turn."),
            HqSetting(key: .effort, label: "Effort", value: info.map { $0.effort ?? "default" } ?? "…", editable: info != nil,
                      hint: "How hard the agent thinks. Kept by Kith for this agent."),
        ]
    }
}

extension BotDraft {
    /// An hq agent's engine, model, effort and tools are kith's (`kith <agent> set …`): the host refuses a change to them
    /// through `updateBot`, so a draft of a kith agent goes back to what the bot holds for those fields. Stale values
    /// in the host's config are never sent as changes.
    public func keepingKithOwned(from bot: Bot) -> BotDraft {
        guard let agent = bot.hqAgent, !agent.isEmpty else { return self }
        var d = self
        d.backend = bot.backend
        d.model = bot.model
        d.modelOptions = bot.modelOptions
        d.fastMode = bot.fastMode
        d.connectors = bot.connectors
        d.skills = bot.skills
        return d
    }
}
