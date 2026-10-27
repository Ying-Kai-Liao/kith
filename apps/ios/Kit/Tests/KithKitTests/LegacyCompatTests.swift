import Foundation
import Testing
@testable import KithKit

/// A dictionary standing in for the keychain.
private final class FakeKeys: KeyStore, @unchecked Sendable {
    var items: [String: Data] = [:]
    var failingWrites = false

    private func key(_ service: String, _ account: String) -> String { "\(service)|\(account)" }
    func read(service: String, account: String) throws -> Data? { items[key(service, account)] }
    func write(service: String, account: String, data: Data) throws {
        if failingWrites { throw Keychain.Failure(status: -1) }
        items[key(service, account)] = data
    }
    func delete(service: String, account: String) { items[key(service, account)] = nil }
}

private let account = "ctx"
private let device = Data("device-key".utf8)
private let push = Data("push-key".utf8)

@Test func oldKeychainItemsMoveToTheNewServices() {
    let store = FakeKeys()
    store.items = ["\(LegacyCompat.oldDeviceService)|\(account)": device, "\(LegacyCompat.oldPushService)|\(account)": push]
    LegacyCompat.migrateKeys(account: account, in: store)
    #expect(store.items == ["\(DeviceIdentity.deviceService)|\(account)": device, "\(DeviceIdentity.pushService)|\(account)": push])
}

@Test func theKeychainMigrationIsIdempotentAndNeverOverwritesANewKey() {
    let store = FakeKeys()
    let newer = Data("newer".utf8)
    store.items = ["\(LegacyCompat.oldDeviceService)|\(account)": device, "\(DeviceIdentity.deviceService)|\(account)": newer]
    LegacyCompat.migrateKeys(account: account, in: store)
    LegacyCompat.migrateKeys(account: account, in: store)
    #expect(store.items == ["\(DeviceIdentity.deviceService)|\(account)": newer])
}

@Test func aFailedCopyKeepsTheOldKeyForTheNextLaunch() {
    let store = FakeKeys()
    store.items = ["\(LegacyCompat.oldDeviceService)|\(account)": device]
    store.failingWrites = true
    LegacyCompat.migrateKeys(account: account, in: store)
    #expect(store.items == ["\(LegacyCompat.oldDeviceService)|\(account)": device])
    store.failingWrites = false
    LegacyCompat.migrateKeys(account: account, in: store)
    #expect(store.items == ["\(DeviceIdentity.deviceService)|\(account)": device])
}

@Test func oldLinksAreMappedToTheCurrentScheme() throws {
    for environment in [AppEnvironment.main, .dev] {
        let old = try #require(URL(string: "\(LegacyCompat.legacyScheme(for: environment))://pair?v=3&id=x"))
        let mapped = LegacyCompat.canonical(old, for: environment)
        #expect(mapped.legacy)
        #expect(mapped.url.absoluteString == "\(environment.urlScheme)://pair?v=3&id=x")
        #expect(environment.accepts(scheme: try #require(mapped.url.scheme)))
        #expect(!environment.accepts(scheme: LegacyCompat.legacyScheme(for: environment)))
        let current = try #require(URL(string: "\(environment.urlScheme)://pair"))
        #expect(!LegacyCompat.canonical(current, for: environment).legacy)
    }
    // Each build only takes the old scheme of its own environment.
    let devOld = try #require(URL(string: "\(LegacyCompat.legacyScheme(for: .dev))://pair"))
    #expect(!LegacyCompat.canonical(devOld, for: .main).legacy)
}

@Test func pairingLinksOfEitherSchemeAreRecognisedWhenScanned() {
    #expect(LegacyCompat.isLegacyLink("\(LegacyCompat.legacyScheme(for: .main))://pair?v=3", for: .main))
    #expect(!LegacyCompat.isLegacyLink("\(AppEnvironment.main.urlScheme)://pair?v=3", for: .main))
}

@Test func labelFamiliesKeepTheirStoredNames() throws {
    #expect(LabelFamily.legacy.rawValue == LegacyCompat.labelPrefix)
    #expect(LabelFamily.kith.rawValue == "kith")
    let encoded = try JSONEncoder().encode([LabelFamily.legacy, .kith])
    #expect(String(decoding: encoded, as: UTF8.self) == "[\"\(LegacyCompat.labelPrefix)\",\"kith\"]")
    #expect(try JSONDecoder().decode([LabelFamily].self, from: encoded) == [.legacy, .kith])
}

@Test func thePushRelayIsTheBuildsHostElseTheOldOne() {
    #expect(SharedStore.relayURL(host: "kith-relay.example.workers.dev") == "https://kith-relay.example.workers.dev")
    #expect(SharedStore.relayURL(host: nil) == LegacyCompat.relayURL)
    #expect(SharedStore.relayURL(host: " ") == LegacyCompat.relayURL)
    #expect(SharedStore.relayURL(host: "$(KITH_PUSH_RELAY_HOST)") == LegacyCompat.relayURL)
}
