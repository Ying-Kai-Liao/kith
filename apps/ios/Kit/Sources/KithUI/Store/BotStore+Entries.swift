import Foundation
import KithKit
import Observation

/// One chat's entries. Its own `@Observable`, so a view reading chat X is invalidated by chat X's writes only.
@Observable
final class ChatEntries {
    var list: [Entry] = []
}

/// Every chat's entries, one `ChatEntries` holder each, created once and kept. The shelf itself is not observed (the
/// store keeps it `@ObservationIgnored`): a read observes only the holders it touches, and `all` (the board, which
/// really wants every chat) also observes `membership`, which moves when a chat gains its first entry or loses its last.
/// A holder is created on read too, so a view can watch a chat that has no entries yet; it stays when the chat empties.
public final class EntryShelf {
    @Observable final class Membership { var generation = 0 }

    private var holders: [String: ChatEntries] = [:]
    private let membership = Membership()

    private func holder(_ id: String) -> ChatEntries {
        if let h = holders[id] { return h }
        let h = ChatEntries()
        holders[id] = h
        return h
    }

    /// The chat's entries, `nil` when it has none (as a missing dictionary key was).
    public subscript(id: String) -> [Entry]? {
        get { let list = holder(id).list; return list.isEmpty ? nil : list }
        set { write(id, newValue ?? []) }
        _modify {
            let h = holder(id)
            var value: [Entry]? = h.list.isEmpty ? nil : h.list
            yield &value
            commit(h, value ?? [])
        }
    }

    public subscript(id: String, default fallback: @autoclosure () -> [Entry]) -> [Entry] {
        get { holder(id).list }
        set { write(id, newValue) }
        _modify {
            let h = holder(id)
            var value = h.list
            yield &value
            commit(h, value)
        }
    }

    private func write(_ id: String, _ list: [Entry]) {
        let h = holder(id)
        if list.isEmpty && h.list.isEmpty { return }
        commit(h, list)
    }

    private func commit(_ h: ChatEntries, _ list: [Entry]) {
        let wasEmpty = h.list.isEmpty
        h.list = list
        if wasEmpty != list.isEmpty { membership.generation += 1 }
    }

    /// Every chat that has entries. Observes all of them: for the board and the cache, not for a row or a chat.
    public var all: [String: [Entry]] {
        _ = membership.generation
        return holders.reduce(into: [:]) { out, pair in
            let list = pair.value.list
            if !list.isEmpty { out[pair.key] = list }
        }
    }

    public var isEmpty: Bool { all.isEmpty }
    public var values: [[Entry]] { Array(all.values) }

    /// Replaces every chat's entries at once (cache load).
    func replaceAll(_ new: [String: [Entry]]) {
        for id in Set(holders.keys).union(new.keys) { write(id, new[id] ?? []) }
    }

    /// Rewrites each chat's entries; a chat left with none drops out.
    func transform(_ change: ([Entry]) -> [Entry]) {
        for (id, h) in holders where !h.list.isEmpty { write(id, change(h.list)) }
    }
}
