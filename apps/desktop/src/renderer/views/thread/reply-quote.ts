// "Reply" on a message (client side): a quote chip above the composer, and the sent text starts with the quoted line
// as a markdown blockquote. The host has no replyTo; the agent just reads the quote.

const MAX = 160

/** A message's first line that says something, cut to a chip's width. */
export function quoteOf(text: string | undefined): string | null {
  const line = (text ?? '').split('\n').map((l) => l.trim()).find(Boolean)
  if (!line) return null
  return line.length > MAX ? `${line.slice(0, MAX - 1).trimEnd()}…` : line
}

/** What goes out: the quote as a "> " blockquote, a blank line, then the reply. */
export function withQuote(text: string, quote: string | null): string {
  return quote ? `> ${quote}\n\n${text}` : text
}

/** The quote waiting above each chat's composer, outside the views so a message row can set it. */
export class ReplyQuotes {
  private readonly quotes = new Map<string, string>()
  private readonly listeners = new Set<() => void>()
  private version = 0

  subscribe = (fn: () => void) => {
    this.listeners.add(fn)
    return () => void this.listeners.delete(fn)
  }
  getVersion = () => this.version
  get(botId: string): string | null {
    return this.quotes.get(botId) ?? null
  }
  set(botId: string, quote: string | null) {
    if (quote) this.quotes.set(botId, quote)
    else if (!this.quotes.delete(botId)) return
    this.version++
    for (const fn of this.listeners) fn()
  }
}

const quotes = new WeakMap<object, ReplyQuotes>()
/** The quotes of a store (one per computer). */
export function quotesOf(store: object): ReplyQuotes {
  let q = quotes.get(store)
  if (!q) quotes.set(store, (q = new ReplyQuotes()))
  return q
}
