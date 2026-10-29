import KithKit
import Foundation
import Observation

/// What a kith bot's detail page shows, fetched through the host (`kithInfo`, serve/docs/bot-info.md).
/// One store per opened details page: a `bot` event doesn't touch it, reopening fetches again, and there is no polling.
@MainActor @Observable
final class KithInfoStore {
    enum Phase {
        case loading
        case loaded(KithInfoResponse)
        /// The host has no `kithInfo` (an older one) or couldn't be reached: the kith sections stay hidden.
        case unavailable
    }

    let botId: String
    private let client: () async throws -> HostClient
    private(set) var phase: Phase = .loading
    private(set) var refreshing = false
    /// Why the last refresh failed while the earlier answer stays on screen.
    private(set) var refreshError: String?
    private var inflight: Task<Void, Never>?
    private var inflightForced = false

    init(botId: String, client: @escaping () async throws -> HostClient) {
        self.botId = botId
        self.client = client
    }

    var info: KithInfo? {
        if case .loaded(let response) = phase { return response.info }
        return nil
    }

    /// Kith couldn't answer: shown as one banner with Retry.
    var failure: String? {
        if case .loaded(let response) = phase, response.info == nil { return response.error }
        return nil
    }

    var isLoading: Bool {
        if case .loading = phase { return true }
        return false
    }

    /// A second call while one runs (a double pull-to-refresh) waits for that one; a forced refresh that
    /// arrives during a plain load runs after it.
    func load(refresh: Bool = false) async {
        if let running = inflight {
            let wasForced = inflightForced
            await running.value
            if !refresh || wasForced { return }
            if let again = inflight {
                await again.value
                return
            }
        }
        #if DEBUG
        if let seeded = DemoAgent.response(for: botId) { phase = .loaded(seeded); return }
        #endif
        let botId = botId
        let task = Task { [client] in
            refreshing = true
            defer { refreshing = false }
            do {
                let response = try await client().kithInfo(botId: botId, refresh: refresh)
                guard !Task.isCancelled else { return }
                if response.info == nil, info != nil {
                    // Kith failed on a refresh: keep the page and say so.
                    refreshError = response.error ?? "kith has no answer"
                } else {
                    refreshError = nil
                    phase = .loaded(response)
                }
            } catch {
                guard !Task.isCancelled else { return }
                // Keep what is on screen when a refresh fails; with nothing yet, hide the sections.
                if case .loaded = phase {} else { phase = .unavailable }
            }
        }
        inflight = task
        inflightForced = refresh
        // The view going away cancels the fetch too.
        await withTaskCancellationHandler { await task.value } onCancel: { task.cancel() }
        if inflight == task { inflight = nil }
    }

    /// Why the last permission change failed; the old value stays shown.
    private(set) var permissionError: String?
    private(set) var savingPermission = false

    /// The host has no `kithInfo` (or couldn't be reached): the kith sections and hq-backed settings stay hidden.
    var unavailable: Bool {
        if case .unavailable = phase { return true }
        return false
    }

    /// Writes the mode through to hq. What shows afterwards is hq's own answer, read back, never the pick itself.
    func setPermission(_ mode: String) async {
        guard !savingPermission else { return }
        savingPermission = true
        permissionError = nil
        defer { savingPermission = false }
        do {
            let response = try await client().kithSetPermission(botId: botId, mode: mode)
            if response.info != nil {
                refreshError = nil
                phase = .loaded(response)
            } else {
                permissionError = response.error ?? "kith has no answer"
            }
        } catch {
            permissionError = error.localizedDescription
        }
    }

    /// Why the last call-model change failed; the old value stays shown.
    private(set) var callModelError: String?
    private(set) var savingCallModel = false

    /// Writes the call model through to hq; what shows afterwards is hq's answer, read back.
    func setCallModel(_ model: String) async {
        guard !savingCallModel else { return }
        savingCallModel = true
        callModelError = nil
        defer { savingCallModel = false }
        do {
            let response = try await client().kithSetCallModel(botId: botId, model: model)
            if response.info != nil {
                refreshError = nil
                phase = .loaded(response)
            } else {
                callModelError = response.error ?? "kith has no answer"
            }
        } catch {
            callModelError = error.localizedDescription
        }
    }

    func search(_ query: String) async throws -> KithSearchResult {
        try await client().kithMemorySearch(botId: botId, query: query, limit: 30)
    }

    func file(_ path: String) async throws -> KithMemoryFile {
        try await client().kithMemoryFile(botId: botId, path: path)
    }
}
