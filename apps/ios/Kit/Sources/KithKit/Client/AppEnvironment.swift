import Foundation

/// Read from the containing app or extension, so the shared package follows its build configuration.
public enum AppEnvironment: String, Sendable {
    case main
    case dev

    public static let current = AppEnvironment(
        rawValue: Bundle.main.object(forInfoDictionaryKey: "KithEnvironment") as? String ?? "main"
    ) ?? .main

    public var appGroup: String { self == .dev ? "group.com.yingkailiao.kith.dev" : "group.com.yingkailiao.kith" }
    public var urlScheme: String { self == .dev ? "kith-dev" : "kith" }
    /// Whether `scheme` is this build's link scheme.
    public func accepts(scheme: String) -> Bool { scheme == urlScheme }

    public func link(_ destination: String) -> URL {
        // All callers supply a fixed app route.
        URL(string: "\(urlScheme)://\(destination)")!
    }
}
