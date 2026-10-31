import Foundation

/// The pure parts of News link pictures (DESIGN.md "News"): what a page's head names as its picture and icon, which
/// hosts the app refuses to ask, and which picture is big enough to show. No network here; `LinkPreviewStore` fetches.
/// Desktop: `link-preview.ts`.
public enum LinkPreview {
    /// The setting under Settings > Privacy > "Show link pictures". On unless turned off.
    public static let enabledKey = "showLinkPictures"
    /// A picture whose short side is under this many pixels counts as no picture (44 pt at 2x).
    public static let minShortSide = 88
    /// Only the start of a page is read: the head sits there.
    public static let maxPageBytes = 512 * 1024
    public static let maxImageBytes = 2 * 1024 * 1024

    public static var isEnabled: Bool { UserDefaults.standard.object(forKey: enabledKey) as? Bool ?? true }

    /// What a page's head points at. Both are absolute http(s) URLs the app may fetch.
    public struct Head: Equatable, Sendable {
        public var image: URL?
        public var icon: URL?
    }

    /// Reads og:image / twitter:image / apple-touch-icon as the picture and the page's icon link (else /favicon.ico) as
    /// the favicon. `base` is the page's URL, for relative links.
    public static func parse(html: String, base: URL) -> Head {
        let head: Substring
        if let end = html.range(of: "</head>", options: .caseInsensitive) { head = html[..<end.lowerBound] } else { head = html[...] }
        var og: String?, twitter: String?, touch: String?, icon: String?, shortcut: String?
        for tag in tags(in: head) {
            let a = tag.attributes
            if tag.name == "meta" {
                let key = (a["property"] ?? a["name"] ?? "").lowercased()
                guard let content = a["content"], !content.isEmpty else { continue }
                if key == "og:image" || key == "og:image:url" || key == "og:image:secure_url" { og = og ?? content }
                if key == "twitter:image" || key == "twitter:image:src" { twitter = twitter ?? content }
            } else if tag.name == "link", let href = a["href"], !href.isEmpty {
                let rels = (a["rel"] ?? "").lowercased().split(whereSeparator: \.isWhitespace).map(String.init)
                if rels.contains("apple-touch-icon") || rels.contains("apple-touch-icon-precomposed") {
                    touch = touch ?? href
                } else if rels.contains("icon") {
                    if rels.contains("shortcut") { shortcut = shortcut ?? href } else { icon = icon ?? href }
                }
            }
        }
        let picture = [og, twitter, touch].lazy.compactMap { resolve($0, against: base) }.first
        let favicon = [icon, shortcut].lazy.compactMap { resolve($0, against: base) }.first
            ?? resolve("/favicon.ico", against: base)
        return Head(image: picture, icon: favicon)
    }

    /// An absolute http(s) URL on a host the app may ask, or nil.
    public static func resolve(_ link: String?, against base: URL) -> URL? {
        guard let link = link?.trimmingCharacters(in: .whitespacesAndNewlines), !link.isEmpty,
              let url = URL(string: link, relativeTo: base)?.absoluteURL else { return nil }
        return isFetchable(url) ? url : nil
    }

    public static func isFetchable(_ url: URL) -> Bool {
        guard let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https",
              let host = url.host(percentEncoded: false), !isPrivate(host: host) else { return false }
        return true
    }

    /// Hosts that name this device or a private network: localhost, .local, single-label names, 0/8, 10/8, 100.64/10,
    /// 127/8, 169.254/16, 172.16/12, 192.168/16, and the IPv6 loopback, unique-local, link-local and mapped forms.
    /// A name that resolves to a private address is not caught (no DNS lookup here).
    public static func isPrivate(host raw: String) -> Bool {
        var host = raw.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "[]."))
        if host.isEmpty { return true }
        if host.contains(":") {
            if let zone = host.firstIndex(of: "%") { host = String(host[..<zone]) }
            if host == "::1" || host == "::" { return true }
            if host.hasPrefix("::ffff:") {
                let tail = String(host.dropFirst(7))
                return tail.contains(".") ? isPrivate(host: tail) : true // hex-form mapped address: refuse
            }
            guard let first = host.split(separator: ":", omittingEmptySubsequences: false).first, !first.isEmpty,
                  let top = UInt16(first, radix: 16) else { return false }
            return top & 0xfe00 == 0xfc00 || top & 0xffc0 == 0xfe80
        }
        let parts = host.split(separator: ".", omittingEmptySubsequences: false)
        if parts.count == 4, parts.allSatisfy({ !$0.isEmpty && $0.allSatisfy(\.isNumber) }) {
            let n = parts.compactMap { Int($0) }
            guard n.count == 4, n.allSatisfy({ $0 <= 255 }) else { return true }
            return n[0] == 0 || n[0] == 10 || n[0] == 127 || (n[0] == 100 && (64...127).contains(n[1]))
                || (n[0] == 169 && n[1] == 254) || (n[0] == 172 && (16...31).contains(n[1])) || (n[0] == 192 && n[1] == 168)
        }
        return parts.count == 1 || host.hasSuffix(".localhost") || host.hasSuffix(".local")
    }

    /// True when a name's resolved addresses must be refused: any of them private, or none at all. This closes the
    /// gap `isPrivate(host:)` leaves for a public name that points at the LAN.
    public static func refuses(addresses: [String]) -> Bool {
        addresses.isEmpty || addresses.contains { isPrivate(host: $0) }
    }

    /// The numeric addresses `host` resolves to (getaddrinfo, off the calling actor).
    public static func addresses(of host: String) async -> [String] {
        await Task.detached {
            var hints = addrinfo(ai_flags: 0, ai_family: AF_UNSPEC, ai_socktype: SOCK_STREAM, ai_protocol: 0,
                                 ai_addrlen: 0, ai_canonname: nil, ai_addr: nil, ai_next: nil)
            var list: UnsafeMutablePointer<addrinfo>?
            guard getaddrinfo(host, nil, &hints, &list) == 0, let first = list else { return [] }
            defer { freeaddrinfo(list) }
            var out: [String] = []
            var node: UnsafeMutablePointer<addrinfo>? = first
            while let n = node {
                var buf = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                if let addr = n.pointee.ai_addr,
                   getnameinfo(addr, n.pointee.ai_addrlen, &buf, socklen_t(buf.count), nil, 0, NI_NUMERICHOST) == 0 {
                    out.append(String(cString: buf))
                }
                node = n.pointee.ai_next
            }
            return out
        }.value
    }

    /// The URL's host resolves only to public addresses.
    static func resolvesPublic(_ url: URL, using resolve: @Sendable (String) async -> [String] = { await addresses(of: $0) }) async -> Bool {
        guard isFetchable(url), let host = url.host(percentEncoded: false) else { return false }
        return !refuses(addresses: await resolve(host))
    }

    // MARK: Tag reading

    struct Tag { var name: String; var attributes: [String: String] }

    private static let tagPattern = try! NSRegularExpression(pattern: "<(meta|link)\\b([^>]*)>", options: [.caseInsensitive])
    private static let attrPattern = try! NSRegularExpression(
        pattern: "([a-zA-Z_:][-a-zA-Z0-9_:.]*)\\s*=\\s*(?:\"([^\"]*)\"|'([^']*)'|([^\\s\"'>]+))", options: [])

    static func tags(in html: Substring) -> [Tag] {
        let text = String(html)
        return tagPattern.matches(in: text, range: NSRange(text.startIndex..., in: text)).compactMap { m in
            guard let name = Range(m.range(at: 1), in: text), let body = Range(m.range(at: 2), in: text) else { return nil }
            let inner = String(text[body])
            var attrs: [String: String] = [:]
            for a in attrPattern.matches(in: inner, range: NSRange(inner.startIndex..., in: inner)) {
                guard let k = Range(a.range(at: 1), in: inner) else { continue }
                let v = (2...4).lazy.compactMap { Range(a.range(at: $0), in: inner) }.first.map { String(inner[$0]) } ?? ""
                let key = inner[k].lowercased()
                if attrs[key] == nil { attrs[key] = unescape(v) }
            }
            return Tag(name: text[name].lowercased(), attributes: attrs)
        }
    }

    private static func unescape(_ s: String) -> String {
        s.replacingOccurrences(of: "&amp;", with: "&").replacingOccurrences(of: "&quot;", with: "\"")
            .replacingOccurrences(of: "&#39;", with: "'").replacingOccurrences(of: "&#x27;", with: "'")
    }
}
