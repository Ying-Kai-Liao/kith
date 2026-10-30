import Foundation
import Testing
@testable import KithKit

@Test func sharedTextWithALinkInsideYieldsThatLink() {
    #expect(SharedLink.firstURL(in: "Look at this https://example.com/a?b=1 it's good")?.absoluteString == "https://example.com/a?b=1")
    #expect(SharedLink.firstURL(in: "https://example.com")?.host() == "example.com")
    #expect(SharedLink.firstURL(in: "just words") == nil)
    // Only web links: an email address or a phone number is not a shared page.
    #expect(SharedLink.firstURL(in: "write to me@example.com or call 555 0100") == nil)
}

@Test func messageHasNoteThenTitleThenLink() {
    let link = URL(string: "https://example.com/post")!
    #expect(SharedLink.message(link: link, title: "A post", text: nil, note: "summarise this")
        == "summarise this\n\nA post\n\nhttps://example.com/post")
    #expect(SharedLink.message(link: link, title: nil, text: nil, note: nil) == "https://example.com/post")
    // Safari often passes the link itself as the shared text; a title that is the address says nothing.
    #expect(SharedLink.message(link: link, title: "https://example.com/post", text: "https://example.com/post", note: " ")
        == "https://example.com/post")
    // A caption that already holds the link isn't repeated beside it.
    #expect(SharedLink.message(link: link, title: nil, text: "Read https://example.com/post now", note: nil)
        == "https://example.com/post")
}

@Test func sharedTextWithoutALinkGoesThroughAsTyped() {
    #expect(SharedLink.message(link: nil, title: nil, text: "  remember the milk ", note: "todo") == "todo\n\nremember the milk")
}

@Test func sharesQueueInOrderAndClearOnlyWhatWasSent() {
    let suite = "share-inbox-\(UUID().uuidString)"
    defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let context = SharedStore.Context(accountID: "a", suite: suite)
    let ref = BotReference(accountId: "a", computerId: "mini", botId: "bot-1")
    let other = BotReference(accountId: "a", computerId: "mini", botId: "bot-2")
    let first = PendingShare(id: "1", target: ref, text: "https://example.com/1")
    let second = PendingShare(id: "2", target: other, text: "https://example.com/2")

    context.queueShare(first)
    context.queueShare(second)
    #expect(SharedStore.Context(accountID: "a", suite: suite).pendingShares.map(\.id) == ["1", "2"])
    #expect(context.lastShareTarget == other)

    context.clearShares(ids: ["1"])
    #expect(context.pendingShares.map(\.id) == ["2"])
    context.clearShares(ids: ["2"])
    #expect(context.pendingShares.isEmpty)
}

@Test func sharesAreIsolatedByAccountAndGoWithErase() {
    let suite = "share-inbox-\(UUID().uuidString)"
    defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let a = SharedStore.Context(accountID: "a", suite: suite)
    let b = SharedStore.Context(accountID: "b", suite: suite)
    a.queueShare(PendingShare(target: BotReference(accountId: "a", computerId: "mini", botId: "bot"), text: "x"))
    #expect(b.pendingShares.isEmpty)
    #expect(b.lastShareTarget == nil)
    a.erase()
    #expect(a.pendingShares.isEmpty)
    #expect(a.lastShareTarget == nil)
}

@Test func aShareExpiresAfterAWeek() {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let ref = BotReference(accountId: nil, computerId: "mini", botId: "bot")
    #expect(!PendingShare(target: ref, text: "x", createdAt: now.addingTimeInterval(-6 * 24 * 3600)).isExpired(at: now))
    #expect(PendingShare(target: ref, text: "x", createdAt: now.addingTimeInterval(-8 * 24 * 3600)).isExpired(at: now))
}
