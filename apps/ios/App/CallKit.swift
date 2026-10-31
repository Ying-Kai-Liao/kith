import AVFAudio
import CallKit
import Foundation
import KithKit
import KithUI
import Observation
import PushKit
import UIKit
import os

private let log = Logger(subsystem: "com.yingkailiao.kith.ios", category: "CallKit")

/// Lets a completion handler cross to the main actor. CallKit and PushKit call these on a queue of their own choosing.
private struct Handoff<T>: @unchecked Sendable { let value: T }

/// The calls the system call UI owns, observable so the in-app incoming-call screen steps aside for them.
@MainActor @Observable
final class OwnedCalls {
    fileprivate(set) var keys: Set<String> = []
    func owns(computerId: ComputerID, callId: String) -> Bool { keys.contains(CallPush.key(computerId: computerId, callId: callId)) }
}

/// An agent's call rings this iPhone like a phone call: a VoIP push (PushKit) wakes the app, and every push is reported to
/// CallKit at once, as iOS requires. Accept and decline come back from the system UI to the store's own
/// accept/decline; the host sends nothing when a call settles, so the ring ends here, from the `call` event or at `deadline`.
@MainActor
final class CallCoordinator: NSObject {
    static let shared = CallCoordinator()

    let owned = OwnedCalls()
    private let provider: CXProvider
    private let controller = CXCallController()
    private var registry: PKPushRegistry?
    private var ledger = CallLedger()
    private var timers: [UUID: Task<Void, Never>] = [:]
    private var backgroundTasks: [UUID: UIBackgroundTaskIdentifier] = [:]

    override init() {
        let config = CXProviderConfiguration()
        config.supportsVideo = false
        config.maximumCallGroups = 1
        config.maximumCallsPerCallGroup = 1
        config.supportedHandleTypes = [.generic]
        // An agent isn't a contact: keep the Phone app's Recents free of them.
        config.includesCallsInRecents = false
        provider = CXProvider(configuration: config)
        super.init()
        provider.setDelegate(self, queue: .main)
    }

    /// Early in launch: a killed app is woken for a VoIP push only if it registers again on every launch.
    func start() {
        guard registry == nil else { return }
        let registry = PKPushRegistry(queue: .main)
        registry.delegate = self
        registry.desiredPushTypes = [.voIP]
        self.registry = registry
    }

    // MARK: ringing

    private func received(_ payload: [AnyHashable: Any], completion: @escaping () -> Void) {
        let push = CallPush.parse(payload)
        let secret = push?.sealed.flatMap { DeviceIdentity.openCall(sealed: $0, computerId: push!.computerId, contextID: push!.ctx) }
        let name = CallPush.callerName(secret)
        let done = Handoff(value: completion)
        if let push, case let .new(uuid) = ledger.add(push) {
            owned.keys.insert(push.key)
            keepAwake(uuid, until: push.deadline)
            // Connect now so the call's own event can say when it stops ringing.
            store(for: push)?.holdCall(push.callId)
            report(uuid, name: name) { [weak self] error in
                Task { @MainActor in
                    if let error {
                        log.error("call not reported: \(error.localizedDescription)")
                        self?.finish(uuid)
                    } else {
                        self?.arm(uuid)
                    }
                    done.value()
                }
            }
        } else {
            // Unreadable, repeated or already over: iOS still wants it reported, so report it and end it at once.
            let uuid = UUID()
            report(uuid, name: name) { [weak self] _ in
                Task { @MainActor in
                    self?.provider.reportCall(with: uuid, endedAt: nil, reason: push == nil ? .failed : .unanswered)
                    done.value()
                }
            }
        }
    }

    private func report(_ uuid: UUID, name: String, completion: @escaping @Sendable (Error?) -> Void) {
        let update = CXCallUpdate()
        update.remoteHandle = CXHandle(type: .generic, value: name)
        update.localizedCallerName = name
        update.hasVideo = false
        update.supportsHolding = false
        update.supportsGrouping = false
        update.supportsUngrouping = false
        update.supportsDTMF = false
        // CallKit has no line for the reason; it is in the app's call screen and the chat.
        provider.reportNewIncomingCall(with: uuid, update: update, completion: completion)
    }

    /// Ends the ring at the host's deadline (it sends nothing when a call settles).
    private func arm(_ uuid: UUID) {
        guard let entry = ledger.entry(uuid) else { return }
        timers[uuid]?.cancel()
        timers[uuid] = Task { [weak self] in
            try? await Task.sleep(for: .seconds(max(0, entry.push.deadline.timeIntervalSinceNow)))
            guard !Task.isCancelled else { return }
            self?.expire()
        }
    }

    private func expire() {
        for uuid in ledger.expired() {
            provider.reportCall(with: uuid, endedAt: nil, reason: .unanswered)
            finish(uuid)
        }
    }

    /// Keeps the app running while it rings, so the link can come up and hear the call settle.
    private func keepAwake(_ uuid: UUID, until deadline: Date) {
        backgroundTasks[uuid] = UIApplication.shared.beginBackgroundTask(withName: "kith-call-ring") { [weak self] in
            Task { @MainActor in self?.releaseBackground(uuid) }
        }
    }

    private func releaseBackground(_ uuid: UUID) {
        guard let task = backgroundTasks.removeValue(forKey: uuid) else { return }
        UIApplication.shared.endBackgroundTask(task)
    }

    /// Forgets a call that is over, whoever ended it.
    private func finish(_ uuid: UUID) {
        timers.removeValue(forKey: uuid)?.cancel()
        releaseBackground(uuid)
        guard let entry = ledger.remove(uuid) else { return }
        owned.keys.remove(entry.push.key)
        store(for: entry.push)?.releaseCall(entry.push.callId)
    }

    // MARK: from the app

    /// A `call` event: a call that stopped ringing (answered elsewhere, declined, missed) ends here too.
    func handle(_ event: IncomingCallEvent, computerId: ComputerID) {
        let key = CallPush.key(computerId: computerId, callId: event.call.id)
        guard let (uuid, reason) = ledger.settle(key: key, event: event) else { return }
        provider.reportCall(with: uuid, endedAt: nil, reason: Self.cxReason(reason))
        finish(uuid)
    }

    /// The Kith voice call for an answered call is over: the system call ends with it.
    func voiceCallEnded(computerId: ComputerID, callId: String) {
        guard let entry = ledger.entry(key: CallPush.key(computerId: computerId, callId: callId)), entry.phase != .ringing else { return }
        let uuid = entry.uuid
        controller.request(CXTransaction(action: CXEndCallAction(call: uuid))) { [weak self] error in
            guard error != nil else { return }
            Task { @MainActor in
                self?.provider.reportCall(with: uuid, endedAt: nil, reason: .remoteEnded)
                self?.finish(uuid)
            }
        }
    }

    static func cxReason(_ reason: CallEndReason) -> CXCallEndedReason {
        switch reason {
        case .answeredElsewhere: .answeredElsewhere
        case .declinedElsewhere: .declinedElsewhere
        case .unanswered: .unanswered
        case .remoteEnded: .remoteEnded
        }
    }

    /// The computer's store, when the call belongs to the signed-in account and a computer it still has.
    private func store(for push: CallPush) -> BotStore? {
        let accounts = AppStore.shared.accounts
        guard push.ctx == accounts.storage.id else { return nil }
        return accounts.store(for: push.computerId)
    }

    // MARK: actions from the system UI

    private func answer(_ action: CXAnswerCallAction) {
        guard let entry = ledger.beginAnswer(action.callUUID) else {
            // A second tap, or a call that is gone.
            action.fulfill()
            return
        }
        guard let store = store(for: entry.push) else {
            action.fail()
            provider.reportCall(with: entry.uuid, endedAt: nil, reason: .failed)
            finish(entry.uuid)
            return
        }
        // CallKit activates the session (`didActivate`) once this is fulfilled; set it up for a voice call first.
        let audio = AVAudioSession.sharedInstance()
        try? audio.setCategory(.playAndRecord, mode: .voiceChat, options: [.allowBluetoothHFP])
        // Fulfilled at once: connecting can outlast CallKit's patience, and a failure ends the call below.
        action.fulfill()
        Task { @MainActor in
            if await store.answerCall(entry.push.callId) {
                ledger.answered(entry.uuid)
                timers.removeValue(forKey: entry.uuid)?.cancel()
                store.dismissCall(entry.push.callId)
                AppStore.shared.openAnsweredCall(AnsweredCallRef(computerId: entry.push.computerId, botId: entry.push.botId, callId: entry.push.callId))
            } else {
                // Another device or the timeout got there first.
                provider.reportCall(with: entry.uuid, endedAt: nil, reason: .answeredElsewhere)
                finish(entry.uuid)
            }
        }
    }

    private func end(_ action: CXEndCallAction) {
        defer { action.fulfill() }
        guard let entry = ledger.entry(action.callUUID) else { return }
        if entry.phase == .ringing {
            store(for: entry.push)?.declineCall(id: entry.push.callId)
        } else {
            // Hung up from the system UI: the Kith voice call goes with it.
            AppStore.shared.closeAnsweredCall(AnsweredCallRef(computerId: entry.push.computerId, botId: entry.push.botId, callId: entry.push.callId))
        }
        finish(entry.uuid)
    }
}

extension CallCoordinator: PKPushRegistryDelegate {
    nonisolated func pushRegistry(_ registry: PKPushRegistry, didUpdate pushCredentials: PKPushCredentials, for type: PKPushType) {
        guard type == .voIP else { return }
        let token = pushCredentials.token
        MainActor.assumeIsolated { PushRegistrar.shared.didRegister(voipToken: token) }
    }

    nonisolated func pushRegistry(_ registry: PKPushRegistry, didInvalidatePushTokenFor type: PKPushType) {
        guard type == .voIP else { return }
        MainActor.assumeIsolated { PushRegistrar.shared.didRegister(voipToken: nil) }
    }

    nonisolated func pushRegistry(_ registry: PKPushRegistry, didReceiveIncomingPushWith payload: PKPushPayload, for type: PKPushType,
                                  completion: @escaping () -> Void) {
        let info = Handoff(value: payload.dictionaryPayload)
        let done = Handoff(value: completion)
        MainActor.assumeIsolated { received(info.value, completion: done.value) }
    }
}

extension CallCoordinator: CXProviderDelegate {
    nonisolated func providerDidReset(_ provider: CXProvider) {
        MainActor.assumeIsolated {
            for uuid in Array(timers.keys) { finish(uuid) }
        }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXAnswerCallAction) {
        let action = Handoff(value: action)
        MainActor.assumeIsolated { answer(action.value) }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXEndCallAction) {
        let action = Handoff(value: action)
        MainActor.assumeIsolated { end(action.value) }
    }

    nonisolated func provider(_ provider: CXProvider, didActivate audioSession: AVAudioSession) {
        // The voice engines start the session themselves (`setActive(true)`), which succeeds now that CallKit holds it.
        log.info("call audio session active")
    }

    nonisolated func provider(_ provider: CXProvider, didDeactivate audioSession: AVAudioSession) {
        log.info("call audio session released")
    }
}
