import Foundation
import Testing
@testable import KithKit

// "kith.example.com$" and "file.example.com$" in Clerk's key encoding.
private let liveKey = "pk_live_" + Data("kith.example.com$".utf8).base64EncodedString()
private let testKey = "pk_test_" + Data("file.example.com$".utf8).base64EncodedString()

@Test func kithVariablesWinOverThePlist() {
    let env = ["KITH_CLERK_PUBLISHABLE_KEY": liveKey, "KITH_CLOUD_URL": "https://kith.example"]
    let kith = AccountConfig.resolve(env: env, cloudURL: "https://file.example", clerkPublishableKey: testKey)
    #expect(kith.clerkPublishableKey == liveKey)
    #expect(kith.cloudURL == URL(string: "https://kith.example"))
    let file = AccountConfig.resolve(env: [:], cloudURL: "https://file.example", clerkPublishableKey: testKey)
    #expect(file.clerkPublishableKey == testKey)
    #expect(file.cloudURL == URL(string: "https://file.example"))
}

@Test func emptyOrMalformedConfigurationMeansAccountsAreOff() {
    let empty = AccountConfig.resolve(env: ["KITH_CLERK_PUBLISHABLE_KEY": "  ", "KITH_CLOUD_URL": ""], cloudURL: "", clerkPublishableKey: "")
    #expect(empty.clerkPublishableKey == nil)
    #expect(empty.cloudURL == nil)
    let bad = AccountConfig.resolve(env: ["KITH_CLERK_PUBLISHABLE_KEY": "sk_live_x", "KITH_CLOUD_URL": "ftp://x.example"], cloudURL: nil, clerkPublishableKey: nil)
    #expect(bad.clerkPublishableKey == nil)
    #expect(bad.cloudURL == nil)
}

@Test func aKeyClerkCouldNotDecodeMeansAccountsAreOff() {
    // The prefix is right but the host inside isn't: Clerk asserts on these when it configures.
    for key in ["pk_live_kith!", "pk_test_", "pk_test_!!!!"] {
        #expect(AccountConfig.resolve(env: [:], cloudURL: nil, clerkPublishableKey: key).clerkPublishableKey == nil, "\(key)")
    }
}

@Test func keysShapedLikeRealOnesAreKept() {
    // Real keys are url-safe base64 without padding; Clerk takes them as they are.
    func key(_ prefix: String, _ host: String) -> String {
        prefix + Data(host.utf8).base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
    for k in [key("pk_live_", "clerk.kith.app$"), key("pk_test_", "quick-fox-12.clerk.accounts.dev$"),
              key("pk_test_", "a\u{e9}-b.example?>$"), key("pk_test_", "no-terminator.example")] {
        #expect(AccountConfig.resolve(env: [:], cloudURL: nil, clerkPublishableKey: k).clerkPublishableKey == k, "\(k)")
    }
    // Both alphabets and padded forms decode the same way Clerk's does.
    let padded = "pk_test_" + Data("clerk.kith.app$".utf8).base64EncodedString()
    #expect(AccountConfig.resolve(env: [:], cloudURL: nil, clerkPublishableKey: padded).clerkPublishableKey == padded)
}

@MainActor @Test func aSetupRunsOnceAndLaterCallersGetTheFirstResult() {
    let once = ConfigureOnce<Int>()
    var runs = 0
    #expect(once.get { runs += 1; return 7 } == 7)
    #expect(once.get { runs += 1; return 8 } == 7)
    #expect(runs == 1)
    // A failed setup isn't remembered.
    let failing = ConfigureOnce<Int>()
    #expect(failing.get { nil } == nil)
    #expect(failing.get { 3 } == 3)
}

@Test func theOAuthReturnFallsBackToTheKithScheme() {
    #expect(AccountConfig.fallbackScheme(environment: "dev") == "kith-dev")
    #expect(AccountConfig.fallbackScheme(environment: "main") == "kith")
    #expect(AccountConfig.fallbackScheme(environment: nil) == "kith")
}

@Test func aLinkCodeDecodesWithItsExpiry() throws {
    let code = try JSONDecoder().decode(LinkCode.self, from: Data(#"{"code":"123456","expiresAt":1760000000000}"#.utf8))
    #expect(code.code == "123456")
    #expect(code.expiry == Date(timeIntervalSince1970: 1_760_000_000))
}

@Test func appleSignInNeedsAMainBuildAndAProductionKey() {
    #expect(AccountConfig.appleSignInAvailable(environment: .main, clerkPublishableKey: "pk_live_x"))
    #expect(!AccountConfig.appleSignInAvailable(environment: .dev, clerkPublishableKey: "pk_live_x"))
    #expect(!AccountConfig.appleSignInAvailable(environment: .main, clerkPublishableKey: "pk_test_x"))
    #expect(!AccountConfig.appleSignInAvailable(environment: .main, clerkPublishableKey: nil))
}
