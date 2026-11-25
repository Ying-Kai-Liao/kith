import KithKit
import Foundation
import Observation

/// The invite sheet's first step: loads what the invite is for. A failed load is not an error the person sees;
/// `generic` is shown instead and joining goes on.
@MainActor
@Observable
public final class InvitePreviewModel {
    public enum Phase: Equatable, Sendable {
        case loading
        case loaded(InvitePreview)
        /// The provider failed: a generic line, same Accept.
        case generic
    }

    public let link: InviteLink
    public private(set) var phase: Phase = .loading
    @ObservationIgnored private let provider: any InvitePreviewProvider
    @ObservationIgnored private var loading = false

    public init(link: InviteLink, provider: any InvitePreviewProvider = MockInvitePreviewProvider()) {
        self.link = link
        self.provider = provider
    }

    /// The loaded preview, if any.
    public var preview: InvitePreview? {
        if case .loaded(let p) = phase { return p }
        return nil
    }

    /// Loads once; asking again while it runs or after it finished does nothing.
    public func load() async {
        guard phase == .loading, !loading else { return }
        loading = true
        defer { loading = false }
        do { phase = .loaded(try await provider.preview(for: link)) } catch { phase = .generic }
    }

    /// The bot the done step opens: one whose id or name matches the first preview item that has a match in `bots`.
    /// Nil when none does (the preview's ids are not guaranteed to be real bots), then the bot list opens instead.
    nonisolated public static func target(for items: [InvitePreviewItem], in bots: [Bot]) -> Bot? {
        for item in items {
            if let bot = bots.first(where: { $0.id == item.id })
                ?? bots.first(where: { $0.name.caseInsensitiveCompare(item.name) == .orderedSame }) {
                return bot
            }
        }
        return nil
    }
}
