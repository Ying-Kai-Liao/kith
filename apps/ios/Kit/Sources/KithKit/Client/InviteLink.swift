import Foundation

/// An invite link, `kith://invite?c=<computerId>&i=<inviteId>&t=<token>` or the page link people paste,
/// `https://<host>/i/<computerId>/<inviteId>#t=<token>` (the token only ever in the fragment).
/// `token` is the secret from the link: it goes to the owner's computer only, never to the cloud and never into a log.
public struct InviteLink: Hashable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    public let computerId: ComputerID
    public let inviteId: String
    public let token: String

    public init(computerId: ComputerID, inviteId: String, token: String) {
        self.computerId = computerId
        self.inviteId = inviteId
        self.token = token
    }

    /// Whatever prints a link (logs, errors, the debugger) must not print the token.
    public var description: String { "InviteLink(computer: \(computerId), invite: \(inviteId))" }
    public var debugDescription: String { description }

    /// Letters, digits, `_` and `-` only, `length` long. Checked by hand: a regex `$` also matches before a trailing newline.
    private static func isSafe(_ s: String, length: ClosedRange<Int>) -> Bool {
        length.contains(s.utf8.count) && s.utf8.allSatisfy {
            ($0 >= 0x30 && $0 <= 0x39) || ($0 >= 0x41 && $0 <= 0x5A) || ($0 >= 0x61 && $0 <= 0x7A) || $0 == 0x5F || $0 == 0x2D
        }
    }

    static func isID(_ s: String) -> Bool { isSafe(s, length: 1...128) }
    static func isToken(_ s: String) -> Bool { isSafe(s, length: 8...512) }

    /// The link's three parts, or nil when the URL isn't an invite or a part is missing or odd.
    public init?(_ url: URL, scheme: String) {
        guard let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let urlScheme = url.scheme?.lowercased() else { return nil }
        let c, i, t: String
        if urlScheme == "https" {
            // A page link, as on desktop: any host, `/i/<c>/<i>` with an optional trailing slash, token from `#t=` only.
            let path = parts.path.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
            guard path.count == 4 || (path.count == 5 && path[4].isEmpty), path[0].isEmpty, path[1] == "i",
                  let fragment = parts.percentEncodedFragment,
                  let token = URLComponents(string: "?" + fragment)?.queryItems?.first(where: { $0.name == "t" })?.value
            else { return nil }
            (c, i, t) = (path[2], path[3], token)
        } else {
            guard urlScheme == scheme.lowercased(), url.host()?.lowercased() == "invite",
                  let items = parts.queryItems else { return nil }
            func value(_ name: String) -> String? { items.first { $0.name == name }?.value }
            guard let cc = value("c"), let ii = value("i"), let tt = value("t") else { return nil }
            (c, i, t) = (cc, ii, tt)
        }
        guard Self.isID(c), Self.isID(i), Self.isToken(t) else { return nil }
        self.init(computerId: c, inviteId: i, token: t)
    }

    /// Whether `url` is shaped like a page invite link (`https://<host>/i/...`), valid or not.
    public static func isPageLink(_ url: URL) -> Bool {
        url.scheme?.lowercased() == "https" && (url.path == "/i" || url.path.hasPrefix("/i/"))
    }

    /// h1: lowercase hex of sha256 of the token's UTF-8 bytes. The cloud only ever sees this.
    public static func proof(of token: String) -> String {
        RelayCrypto.sha256(Data(token.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    public var proof: String { Self.proof(of: token) }

    public static func isProof(_ s: String) -> Bool {
        s.utf8.count == 64 && s.utf8.allSatisfy { ($0 >= 0x30 && $0 <= 0x39) || ($0 >= 0x61 && $0 <= 0x66) }
    }
}

/// What the app keeps of an invite so a reconnect or a new pairing can prove it again.
public struct StoredInvite: Codable, Equatable, Sendable, CustomStringConvertible {
    public var inviteId: String
    public var token: String

    public init(inviteId: String, token: String) {
        self.inviteId = inviteId
        self.token = token
    }

    public var description: String { "StoredInvite(invite: \(inviteId))" }
}

/// Where invite tokens are kept, by computer.
public protocol InviteTokenStore: Sendable {
    func stored(for computer: ComputerID) -> StoredInvite?
    /// Throws when the token can't be stored safely: the join must not go on without it.
    func keep(_ invite: StoredInvite, for computer: ComputerID) throws
    func forget(_ computer: ComputerID)
}

/// Tokens in the Keychain (this device only), one per account context and computer.
public struct KeychainInviteStore: InviteTokenStore {
    static let service = "com.yingkailiao.kith.invite-token"
    let context: String

    public init(context: SharedStore.Context) { self.context = context.id }

    private func account(_ computer: ComputerID) -> String { "\(context)/\(computer)" }

    public func stored(for computer: ComputerID) -> StoredInvite? {
        guard let data = try? Keychain.read(service: Self.service, account: account(computer)) else { return nil }
        return try? JSONDecoder().decode(StoredInvite.self, from: data)
    }

    public func keep(_ invite: StoredInvite, for computer: ComputerID) throws {
        let data = try JSONEncoder().encode(invite)
        // Replace: a write over an existing item would leave the old invite in place.
        Keychain.delete(service: Self.service, account: account(computer))
        try Keychain.write(service: Self.service, account: account(computer), data: data)
    }

    public func forget(_ computer: ComputerID) {
        Keychain.delete(service: Self.service, account: account(computer))
    }
}
