import Foundation

/// Parsed Markdown by source text. A chat row is built again whenever its cell scrolls back in (and every row of a
/// chat when anything in it changes), so the parse of a long reply was redone each time; the text itself rarely changes.
/// A reply still streaming misses once per token, which costs what it did before.
enum MarkdownCache {
    private final class Blocks { let value: [MarkdownBlocks.Block]; init(_ v: [MarkdownBlocks.Block]) { value = v } }
    private final class Inline { let value: AttributedString; init(_ v: AttributedString) { value = v } }

    // NSCache is thread-safe; Swift just can't see it.
    private nonisolated(unsafe) static let blockCache: NSCache<NSString, Blocks> = {
        let c = NSCache<NSString, Blocks>()
        c.countLimit = 300
        return c
    }()
    private nonisolated(unsafe) static let inlineCache: NSCache<NSString, Inline> = {
        let c = NSCache<NSString, Inline>()
        c.countLimit = 3000
        return c
    }()

    /// `parse` is the real parser; a test passes its own to count how often it runs.
    static func blocks(_ source: String, cached: Bool = true, parse: (String) -> [MarkdownBlocks.Block] = MarkdownBlocks.parse) -> [MarkdownBlocks.Block] {
        // A reply still being written is a new string every token: keeping each would only push real replies out.
        guard cached else { return parse(source) }
        let key = source as NSString
        if let hit = blockCache.object(forKey: key) { return hit.value }
        let parsed = parse(source)
        blockCache.setObject(Blocks(parsed), forKey: key)
        return parsed
    }

    static func inline(_ source: String, cached: Bool = true, parse: (String) -> AttributedString) -> AttributedString {
        guard cached else { return parse(source) }
        let key = source as NSString
        if let hit = inlineCache.object(forKey: key) { return hit.value }
        let parsed = parse(source)
        inlineCache.setObject(Inline(parsed), forKey: key)
        return parsed
    }
}
