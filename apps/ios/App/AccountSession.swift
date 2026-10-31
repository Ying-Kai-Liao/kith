import AuthenticationServices
import ClerkKit
import KithKit
import KithUI
import Foundation
import Observation

/// Clerk owns OAuth, session restoration and Keychain persistence. An account session
/// never grants access to a computer by itself: each one still approves this device (spec §4.2).
@MainActor
@Observable
final class AccountSession {
    private let clerk: Clerk?
    /// Main builds on the production Clerk instance only; Dev and Personal Team builds can't sign in with Apple.
    let showsAppleSignIn: Bool
    private(set) var isBusy = false
    var errorMessage: String?

    var showSwitcher = false

    struct Account: Identifiable {
        let id: String
        let sessionID: String
        let email: String
        let avatarURL: URL?
    }

    var userID: String? { clerk?.session?.status == .active ? clerk?.user?.id : nil }
    var accounts: [Account] {
        var seen = Set<String>()
        return (clerk?.auth.sessions ?? []).compactMap { session in
            guard session.status == .active, session.expireAt > .now,
                  let user = session.user, seen.insert(user.id).inserted else { return nil }
            return Account(id: user.id, sessionID: session.id,
                           email: user.primaryEmailAddress?.emailAddress ?? "Kith account",
                           avatarURL: URL(string: user.imageUrl).flatMap { $0.scheme == "https" ? $0 : nil })
        }
    }
    var supportsMultipleAccounts: Bool { clerk?.environment?.authConfig.singleSessionMode == false }

    var isConfigured: Bool { clerk != nil }
    /// Clerk restores the persisted Keychain session during configuration. Don't offer a fresh sign-in
    /// until that restore has completed, or an update can briefly look like a signed-out install.
    var isReady: Bool { clerk?.isLoaded ?? true }
    /// The Kith cloud (accounts and the encrypted relay); nil in builds without one.
    let cloudURL: URL?
    var isSignedIn: Bool { userID != nil }
    var email: String? { clerk?.user?.primaryEmailAddress?.emailAddress }
    var avatarURL: URL? {
        guard let image = clerk?.user?.imageUrl, let url = URL(string: image), url.scheme == "https" else { return nil }
        return url
    }

    /// Each external account's own picture (Google, GitHub) that can become the profile image.
    var providerImages: [ProfileImageSource] {
        ProfileImageSource.sources(from: (clerk?.user?.externalAccounts ?? []).map { ($0.provider, $0.imageUrl) })
    }

    /// Sets the active account's profile image in Clerk, then reloads the user so every avatar updates.
    func setProfileImage(_ data: Data) async -> Bool {
        guard let user = clerk?.user, isSignedIn else { return false }
        do {
            _ = try await user.setProfileImage(imageData: data)
            _ = try await user.reload()
            return true
        } catch {
            return false
        }
    }

    init() {
        let env = ProcessInfo.processInfo.environment
        let config = Bundle.main.url(forResource: "AccountConfig", withExtension: "plist")
            .flatMap { try? Data(contentsOf: $0) }
            .flatMap { try? PropertyListDecoder().decode(Configuration.self, from: $0) }
        let resolved = AccountConfig.resolve(env: env, cloudURL: config?.cloudURL, clerkPublishableKey: config?.clerkPublishableKey)
        cloudURL = resolved.cloudURL
        showsAppleSignIn = AccountConfig.appleSignInAvailable(environment: .current, clerkPublishableKey: resolved.clerkPublishableKey)
        guard let key = resolved.clerkPublishableKey else {
            // Empty or malformed: sign-in is unavailable. Clerk would assert on a key it can't decode.
            if let raw = config?.clerkPublishableKey ?? env["KITH_CLERK_PUBLISHABLE_KEY"], !raw.isEmpty {
                NSLog("Kith: Clerk publishable key is malformed; sign-in is unavailable")
            }
            clerk = nil
            return
        }
        let scheme = Bundle.main.bundleIdentifier ?? AccountConfig.fallbackScheme(
            environment: Bundle.main.object(forInfoDictionaryKey: "KithEnvironment") as? String)
        clerk = Self.configured.get {
            Clerk.configure(publishableKey: key, options: .init(
                telemetryEnabled: false,
                redirectConfig: .init(redirectUrl: "\(scheme)://callback", callbackUrlScheme: scheme)
            ))
        }
    }

    /// Clerk is one per process; a second session (re-created, or built by a background launch) shares it.
    private static let configured = ConfigureOnce<Clerk>()

    /// The signed-in session's JWT for the Kith cloud (Clerk caches it for a minute).
    func sessionToken() async throws -> String {
        guard let clerk, isSignedIn, let token = try await clerk.auth.getToken() else { throw SessionError.signedOut }
        return token
    }

    /// The Kith cloud for `userID`'s context; nil when signed out or the build has no cloud.
    /// Requests stop working once another account becomes active, so one account's calls never carry another's token.
    func cloudClient(for userID: String?, identity: DeviceIdentity?) -> CloudClient? {
        guard let userID, let cloudURL, isConfigured else { return nil }
        return CloudClient(baseURL: cloudURL, identity: identity) { [weak self] in
            guard let self, await self.userID == userID else { throw SessionError.signedOut }
            return try await self.sessionToken()
        }
    }

    enum SessionError: LocalizedError {
        case signedOut
        var errorDescription: String? { "Sign in again to reach your account." }
    }

    enum SignInProvider: String {
        case google = "Google"
        case apple = "Apple"
        case github = "GitHub"
    }

    func signIn(provider: SignInProvider = .google) async {
        guard !isBusy else { return }
        guard let clerk else {
            errorMessage = "Sign-in setup isn't finished yet. You can continue using local pairing."
            return
        }
        isBusy = true
        errorMessage = nil
        defer { isBusy = false }
        do {
            // All providers use Clerk's transfer flow and activate the same kind of session.
            let result: TransferFlowResult
            switch provider {
            case .google:
                result = try await clerk.auth.signInWithOAuth(provider: .google, prefersEphemeralWebBrowserSession: isSignedIn)
            case .github:
                result = try await clerk.auth.signInWithOAuth(provider: .github, prefersEphemeralWebBrowserSession: isSignedIn)
            case .apple:
                result = try await clerk.auth.signInWithApple()
            }
            switch result {
            case .signIn(let signIn):
                if signIn.status != .complete {
                    errorMessage = "\(provider.rawValue) sign-in needs additional verification. Your account is not signed in yet."
                }
            case .signUp(let signUp):
                if signUp.status != .complete {
                    errorMessage = "\(provider.rawValue) sign-up needs additional account information. Your account is not signed in yet."
                }
            }
            if errorMessage == nil { Analytics.shared.capture(.signedIn) }
        } catch let error as ASWebAuthenticationSessionError where error.code == .canceledLogin {
            return
        } catch let error as ASAuthorizationError where error.code == .canceled {
            return
        } catch is CancellationError {
            return
        } catch {
            errorMessage = "Couldn't sign in. Check your connection and try again."
        }
    }

    func signOut() async {
        guard !isBusy, let clerk, let sessionID = clerk.session?.id else { return }
        isBusy = true
        errorMessage = nil
        defer { isBusy = false }
        do {
            try await clerk.auth.signOut(sessionId: sessionID)
        } catch {
            errorMessage = "Couldn't sign out. Check your connection and try again."
        }
    }

    /// Every account on this device, for starting over.
    func signOutAll() async {
        guard let clerk else { return }
        isBusy = true
        errorMessage = nil
        defer { isBusy = false }
        for session in clerk.auth.sessions {
            try? await clerk.auth.signOut(sessionId: session.id)
        }
    }

    func switchAccount(_ account: Account) async {
        guard !isBusy, let clerk, account.id != userID else { return }
        isBusy = true
        errorMessage = nil
        defer { isBusy = false }
        do {
            try await clerk.auth.setActive(sessionId: account.sessionID)
        } catch {
            errorMessage = "Couldn't switch accounts. Check your connection or sign in again."
        }
    }

    func handle(_ url: URL) async {
        guard let clerk else { return }
        do { try await clerk.handle(url) }
        catch { errorMessage = "Couldn't finish signing in. Please try again." }
    }

    /// `AccountConfig.plist` (`KITH_CLERK_PUBLISHABLE_KEY` / `KITH_CLOUD_URL` override it).
    private struct Configuration: Decodable {
        let clerkPublishableKey: String?
        let cloudURL: String?
    }
}
