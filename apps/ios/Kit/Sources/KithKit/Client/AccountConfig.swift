import Foundation

/// The Clerk key and cloud URL a build runs with. `KITH_*` environment variables win over the build's
/// `AccountConfig.plist`. Empty or malformed means off.
public enum AccountConfig {
    public struct Resolved: Equatable, Sendable {
        public var clerkPublishableKey: String?
        public var cloudURL: URL?
    }

    public static func resolve(env: [String: String], cloudURL: String?, clerkPublishableKey: String?) -> Resolved {
        func pick(_ values: [String?]) -> String {
            values.compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }.first { !$0.isEmpty } ?? ""
        }
        let key = pick([env["KITH_CLERK_PUBLISHABLE_KEY"], clerkPublishableKey])
        let cloud = pick([env["KITH_CLOUD_URL"], cloudURL])
        return Resolved(
            clerkPublishableKey: isClerkKey(key) ? key : nil,
            cloudURL: URL(string: cloud).flatMap { Pairing.isCloudURL($0) ? $0 : nil })
    }

    /// Clerk's own check, plus the decode it makes later and answers with a crash. Mirrors ClerkKit's
    /// `String.base64String()`: the part after the prefix is url-safe base64 (padding optional) of UTF-8 text, the
    /// frontend API host. Deliberately no stricter than Clerk (it drops the trailing `$` without checking it), so a key
    /// Clerk accepts is never turned off here.
    static func isClerkKey(_ key: String) -> Bool {
        guard let prefix = ["pk_test_", "pk_live_"].first(where: key.hasPrefix) else { return false }
        var body = String(key.dropFirst(prefix.count))
            .replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        if body.count % 4 != 0 { body += String(repeating: "=", count: 4 - body.count % 4) }
        guard let data = Data(base64Encoded: body), let host = String(data: data, encoding: .utf8) else { return false }
        return !host.isEmpty
    }

    /// Apple sign-in needs the Sign in with Apple capability (Main builds) and the production Clerk instance; the dev
    /// instance has no Apple credentials of its own.
    public static func appleSignInAvailable(environment: AppEnvironment, clerkPublishableKey: String?) -> Bool {
        environment == .main && clerkPublishableKey?.hasPrefix("pk_live_") == true
    }

    /// The OAuth return scheme when the bundle has no identifier (it always does in a real app).
    public static func fallbackScheme(environment: String?) -> String {
        environment == "dev" ? "kith-dev" : "kith"
    }
}
