import Foundation

/// What a writer-change divider says (`data.writerChange`). Unknown words are `.other`.
public enum WriterChange: Hashable, Sendable {
    case startedInTerminal, openedInTerminal, tookOver, terminalClosed
    case other(String)

    public init?(_ raw: String?) {
        switch raw {
        case nil: return nil
        case "startedInTerminal": self = .startedInTerminal
        case "openedInTerminal": self = .openedInTerminal
        case "tookOver": self = .tookOver
        case "terminalClosed": self = .terminalClosed
        case let raw?: self = .other(raw)
        }
    }
}

public extension EntryData {
    var writerChangeKind: WriterChange? { WriterChange(writerChange) }
}

/// What a queued message of the user's says under it.
public enum UserStatusLine {
    /// It waits for a terminal that holds its thread, for the bot's running turn, or just for its turn.
    public static func queued(waitsFor: String?, botWorking: Bool) -> String {
        if waitsFor == "terminal" { return "Waits until the terminal session ends" }
        return botWorking ? "Queued until this response finishes" : "Queued"
    }
}

/// What `takeOverThread` answers. `tookOver` is false when there was nothing to take (already closed, or a second tap).
public struct TakeOverResult: Codable, Hashable, Sendable {
    public var liveWriter: String?
    public var tookOver: Bool
}

/// A refusal about who holds a thread. The host's message starts with the code (HTTP 409).
public enum ThreadWriterError: LocalizedError, Hashable, Sendable {
    /// Send to a thread open in a terminal.
    case liveInTerminal(String)
    /// Send to a thread some other writer holds.
    case heldByOtherWriter(String)
    /// Take-over while the terminal is working, or it didn't close.
    case terminalBusy(String)

    public init?(_ error: Error) {
        guard case let HostError.http(409, message) = error else { return nil }
        if message.hasPrefix("liveInTerminal") { self = .liveInTerminal(message) }
        else if message.hasPrefix("heldByOtherWriter") { self = .heldByOtherWriter(message) }
        else if message.hasPrefix("terminalBusy") { self = .terminalBusy(message) }
        else { return nil }
    }

    public var errorDescription: String? {
        switch self {
        case let .liveInTerminal(m), let .heldByOtherWriter(m), let .terminalBusy(m):
            m.split(separator: ":", maxSplits: 1).last.map { $0.trimmingCharacters(in: .whitespaces) } ?? m
        }
    }
}

public extension HostClient {
    /// Takes a thread over from the terminal that holds it: the host ends the terminal when it is idle.
    /// Throws `ThreadWriterError.terminalBusy` while the terminal works. Owner only.
    func takeOverThread(botId: String, threadId: String) async throws -> TakeOverResult {
        struct Body: Encodable { var botId: String; var threadId: String }
        do {
            return try await call("takeOverThread", Body(botId: botId, threadId: threadId), timeout: 30)
        } catch {
            throw ThreadWriterError(error) ?? error
        }
    }
}
