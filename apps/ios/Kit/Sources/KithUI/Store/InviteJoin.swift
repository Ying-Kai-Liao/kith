import KithKit
import Foundation

/// The invitee's side of an invite link, as steps the app can show.
public enum InviteJoinStep: Equatable, Sendable {
    case signingIn, joining, asking, connecting, proving, done
    /// Paused until the computer is reachable; finishes by itself when it is.
    case waiting
    case failed
}

public struct InviteJoinState: Equatable, Sendable {
    public var step: InviteJoinStep
    /// The words for `failed` and `waiting`.
    public var message: String?
    /// The computer the invite is for.
    public var computerId: ComputerID?

    public init(_ step: InviteJoinStep, computerId: ComputerID? = nil, message: String? = nil) {
        self.step = step
        self.computerId = computerId
        self.message = message
    }

    public var isFinished: Bool { step == .done || step == .failed }
}

/// What the join needs from the app; faked in tests.
@MainActor
public protocol InviteJoinDeps {
    var isSignedIn: Bool { get }
    /// Returns once the account is signed in (the person picks Apple, Google or GitHub on the sheet).
    func waitForSignIn() async throws
    /// `POST /v1/invites/<id>/accept` with `{token: h1}`.
    func accept(inviteId: String, proof: String) async throws -> InviteAccepted
    /// Keeps the token for this computer so a reconnect can prove again. Throws when it can't be stored.
    func keep(_ invite: StoredInvite, for computer: ComputerID) throws
    /// Asks the computer for access (`POST /v1/computers/<c>/access-requests`); asking twice is harmless.
    /// False while the computer is offline: it is asked again when it is back. The request's six digits are in
    /// `AccountStore.pendingAccess` until it is answered: the join shows them (`InviteJoin.codeLine`) while it waits.
    func requestAccess(_ computer: ComputerID) async throws -> Bool
    /// The connection to that computer once access is granted (it waits for the answer), or nil while it is offline.
    func connect(_ computer: ComputerID) async throws -> (any InviteHost)?
}

public enum InviteJoin {
    /// The words for a waiting device's six digits, the same as on desktop: in Code mode the computer's owner
    /// compares them before letting the device in, a member's too.
    public static func codeLine(_ code: String) -> String {
        "Your code: \(code.prefix(3)) \(code.suffix(3)). The computer's owner may ask for these digits."
    }

    static func words(_ error: Error, fallback: String) -> String {
        let text = error.localizedDescription
        return text.isEmpty ? fallback : text
    }

    /// The cloud's own words for 403, 404, 409 and 410; a failed link gets a plain line.
    static func acceptText(_ error: Error) -> String {
        if let e = error as? CloudError {
            if e.status == 0 && e.code != "badInvite" { return "Can't reach the Kith cloud. Check your connection." }
            return e.errorDescription ?? "Couldn't join. Try the link again."
        }
        if error is URLError { return "Can't reach the Kith cloud. Check your connection." }
        return words(error, fallback: "Couldn't join. Try the link again.")
    }

    /// Runs the whole join and reports each step. Safe to run again after `waiting`: accepting answers
    /// the same membership for the same account, and the token is proved again.
    @MainActor
    @discardableResult
    public static func run(_ link: InviteLink, deps: some InviteJoinDeps, report: (InviteJoinState) -> Void) async -> InviteJoinState {
        func end(_ state: InviteJoinState) -> InviteJoinState { report(state); return state }
        func failed(_ message: String, _ id: ComputerID?) -> InviteJoinState { end(InviteJoinState(.failed, computerId: id, message: message)) }

        if !deps.isSignedIn {
            report(InviteJoinState(.signingIn, computerId: link.computerId))
            do { try await deps.waitForSignIn() } catch { return failed(words(error, fallback: "Sign-in was cancelled."), link.computerId) }
        }

        report(InviteJoinState(.joining, computerId: link.computerId))
        var computer = link.computerId
        do {
            // Only h1 leaves for the cloud; the token stays here and goes to the computer.
            let joined = try await deps.accept(inviteId: link.inviteId, proof: link.proof)
            if !joined.computerId.isEmpty { computer = joined.computerId }
        } catch {
            return failed(acceptText(error), link.computerId)
        }
        do {
            try deps.keep(StoredInvite(inviteId: link.inviteId, token: link.token), for: computer)
        } catch {
            // Never go on without a safe place for the token: a reconnect couldn't prove again.
            return failed("Can't store this invite safely on this iPhone, so it wasn't used. Check your passcode and try the link again.", computer)
        }

        report(InviteJoinState(.asking, computerId: computer))
        do {
            guard try await deps.requestAccess(computer) else {
                return end(InviteJoinState(.waiting, computerId: computer, message: "The computer is offline. This finishes when it is back online."))
            }
        } catch {
            if (error as? CloudError).map({ $0.isTransient || $0.status == 0 }) ?? (error is URLError) {
                return end(InviteJoinState(.waiting, computerId: computer, message: "Couldn't reach the Kith cloud. This finishes when you open Kith again."))
            }
            return failed(words(error, fallback: "Couldn't ask the computer for access."), computer)
        }

        report(InviteJoinState(.connecting, computerId: computer))
        let host: (any InviteHost)?
        do { host = try await deps.connect(computer) } catch {
            return failed(words(error, fallback: "Couldn't connect to the computer."), computer)
        }
        guard let host else {
            return end(InviteJoinState(.waiting, computerId: computer, message: "The computer isn't reachable. This finishes when it is back online."))
        }

        report(InviteJoinState(.proving, computerId: computer))
        do {
            // The first call to the computer; until it succeeds, it refuses everything else.
            try await host.proveInvite(inviteId: link.inviteId, token: link.token)
        } catch {
            if InviteProof.isAlreadyProven(error) { return end(InviteJoinState(.done, computerId: computer)) }
            if let e = error as? HostError, e.isTransient {
                return end(InviteJoinState(.waiting, computerId: computer, message: "Lost the connection to the computer. This finishes when it is back."))
            }
            return failed(words(error, fallback: "The computer didn't accept this invite."), computer)
        }
        return end(InviteJoinState(.done, computerId: computer))
    }
}
