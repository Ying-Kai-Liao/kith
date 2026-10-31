import Foundation

/// Everything that still answers to the app's old name, in one place. Kith's apps were Codync's
/// before the rename; installs, QR codes and paired computers from then keep working through this
/// file alone. Nothing here is produced anew: new links, labels and keychain items use Kith names.
public enum LegacyCompat {
    // MARK: Link schemes

    /// The scheme links used before the rename (`codync://pair?...`): opened, never generated.
    /// The Info.plist registers the same values from `KITH_LEGACY_URL_SCHEME`.
    public static func legacyScheme(for environment: AppEnvironment) -> String {
        environment == .dev ? "codync-dev" : "codync"
    }

    /// `url` with an old scheme swapped for this build's own, so one handler serves both; `legacy` says
    /// it was an old link (an old computer's pairing keeps speaking the old crypto labels).
    public static func canonical(_ url: URL, for environment: AppEnvironment = .current) -> (url: URL, legacy: Bool) {
        guard url.scheme == legacyScheme(for: environment),
              var parts = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return (url, false) }
        parts.scheme = environment.urlScheme
        return (parts.url ?? url, true)
    }

    /// Whether scanned or pasted text starts like an old link of this build.
    public static func isLegacyLink(_ text: String, for environment: AppEnvironment = .current) -> Bool {
        text.hasPrefix(legacyScheme(for: environment) + "://")
    }

    // MARK: Relay protocol labels

    /// The domain-separation prefix of pairings made before the rename: `codync/*` labels, the
    /// `codync-sig-v1` signature prefix. The host still accepts it.
    public static let labelPrefix = "codync"

    // MARK: Live URL

    /// The APNs push relay under its old name, used when the build names none (`KITH_PUSH_RELAY_HOST`).
    /// Delete once the builds point at kith-relay (decision q25).
    public static let relayURL = "https://kith-relay.lykevin890919.workers.dev"

    // MARK: Stored data

    /// Keychain services of the keys that identify this install to its hosts. Renaming the service
    /// without moving the item would orphan the paired identity.
    static let oldDeviceService = "com.pokai.Codync.device-key"
    static let oldPushService = "com.pokai.Codync.push-key"

    /// Files in the caches folder named before the rename (a mirror per computer and account).
    static let oldMirrorPrefix = "codync-mirror-"

    /// Moves the device and push keys from their old keychain services to the new ones: copy when the
    /// new item is missing, then drop the old one. Safe to run on every launch and from any process
    /// of the keychain group; with both present it only deletes the old copy.
    static func migrateKeys(account: String, in store: some KeyStore) {
        for (old, new) in [(oldDeviceService, DeviceIdentity.deviceService), (oldPushService, DeviceIdentity.pushService)] {
            // Any failure leaves the old item in place: the next launch tries again.
            guard let data = try? store.read(service: old, account: account) else { continue }
            let existing: Data?
            do { existing = try store.read(service: new, account: account) } catch { continue }
            if existing == nil {
                guard (try? store.write(service: new, account: account, data: data)) != nil else { continue }
            }
            store.delete(service: old, account: account)
        }
    }
}

/// The slice of the keychain the migration needs, so a test can stand in for it.
protocol KeyStore {
    func read(service: String, account: String) throws -> Data?
    func write(service: String, account: String, data: Data) throws
    func delete(service: String, account: String)
}
