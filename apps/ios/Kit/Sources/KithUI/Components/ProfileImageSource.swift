import Foundation

/// A sign-in provider's own picture for the account, offered as a profile image.
public struct ProfileImageSource: Identifiable, Equatable, Sendable {
    public let provider: String
    public let url: URL
    public var id: String { provider + url.absoluteString }

    /// "Google", "GitHub"; Clerk reports providers as "google" or "oauth_google".
    public var title: String {
        let name = provider.replacingOccurrences(of: "oauth_", with: "")
        switch name {
        case "github": return "GitHub"
        case "": return "Provider"
        default: return name.prefix(1).uppercased() + name.dropFirst()
        }
    }

    public init(provider: String, url: URL) {
        self.provider = provider
        self.url = url
    }

    /// One source per external account that has an https picture; the rest are hidden.
    public static func sources(from accounts: [(provider: String, imageURL: String?)]) -> [ProfileImageSource] {
        accounts.compactMap { account in
            guard let raw = account.imageURL, !raw.isEmpty, let url = URL(string: raw),
                  url.scheme == "https" else { return nil }
            return ProfileImageSource(provider: account.provider, url: url)
        }
    }
}
