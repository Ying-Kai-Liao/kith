import type { HostClient } from '../client/host-client'
import { Observable } from '../lib/observable'
import {
  movedSince, previousVersion, type HqMemoryList, type HqMemoryRead, type HqMemoryVersion, type HqTidyUp, type HqUndoResult,
} from '@shared/hq-memory'
import { isUnsupportedHost } from '@shared/kith-info'

/** Edits left open when the sheet went away (another bot, another tab), by file, with the text they were made over. */
const unsaved = new Map<string, { base: string; draft: string }>()

const OLDER_HOST = "Update Kith on your computer to see this agent's memory."

/** An hq agent's memory: its files, the shared user files and its tidy-ups (`kithMemory*`). */
export class HqMemoryStore extends Observable {
  list: HqMemoryList | null = null
  tidyUps: HqTidyUp[] = []
  tidyLoaded = false
  /** Why the tidy-ups couldn't be read; the files still show. */
  tidyError: string | null = null
  /** Why the memory can't be shown (an older host, the agent's folder gone); null while it can. */
  error: string | null = null
  busy = false
  /** What the last undo did, with the run it was for. */
  undone: { id: number; result: HqUndoResult } | null = null
  undoError: string | null = null
  /** The fact the last "Forget a fact" kept out of memory; the error when it couldn't. */
  forgot: string | null = null
  forgetError: string | null = null
  private request = 0

  constructor(readonly botId: string, private readonly client: () => Promise<HostClient>) { super() }

  /** Keeps a fact out of memory for good. Returns whether it worked, so the input can clear. */
  async forget(text: string): Promise<boolean> {
    const fact = text.trim()
    if (this.busy || !fact) return false
    this.busy = true
    this.forgot = null
    this.forgetError = null
    this.changed()
    try {
      await (await this.client()).hqMemoryForget(this.botId, fact)
      this.forgot = fact
      return true
    } catch (error) {
      this.forgetError = message(error)
      return false
    } finally {
      this.busy = false
      this.changed()
    }
  }

  async load() {
    const request = ++this.request
    try {
      const c = await this.client()
      const [list, tidyUps] = await Promise.all([
        c.hqMemoryList(this.botId),
        // Tidy-ups are a side panel: their failure must not hide the files.
        c.hqTidyUps(this.botId).then((runs) => ({ runs }), (error) => ({ error: message(error) })),
      ])
      if (request !== this.request) return
      this.list = list
      this.error = null
      if ('runs' in tidyUps) { this.tidyUps = tidyUps.runs; this.tidyLoaded = true; this.tidyError = null }
      else this.tidyError = tidyUps.error
    } catch (error) {
      if (request === this.request) this.error = isUnsupportedHost(error) ? OLDER_HOST : message(error)
    }
    this.changed()
  }

  /** Puts a tidy-up back; `refused` lists the files that changed since and were left alone. */
  async undo(id: number) {
    if (this.busy) return
    this.busy = true
    this.undoError = null
    this.changed()
    try {
      const result = await (await this.client()).hqUndoTidy(this.botId, id)
      this.undone = { id, result }
      await this.load()
    } catch (error) {
      this.undoError = message(error)
    } finally {
      this.busy = false
      this.changed()
    }
  }
}

/** One memory file open for editing: its text, history and the guards around a save. */
export class HqFileEditor extends Observable {
  read: HqMemoryRead | null = null
  draft = ''
  versions: HqMemoryVersion[] = []
  /** The newest version when the file was opened: a save over a newer one is held back. */
  openedNewest: number | null = null
  /** The newest version moved since the file was opened; the draft waits for the user's choice. */
  conflict = false
  warnings: string[] = []
  error: string | null = null
  loading = true
  busy = false
  /** The draft is an unsaved edit brought back from before. */
  restored = false
  diffs = new Map<number, { diff: string } | { error: string } | 'loading'>()
  private request = 0

  constructor(readonly botId: string, readonly shared: boolean, readonly name: string, private readonly client: () => Promise<HostClient>) { super() }

  private get key() { return `${this.botId}:${this.shared}:${this.name}` }

  /** Keeps an unsaved edit for the next time the file is opened. */
  stash() {
    if (this.read && this.dirty && !this.readOnly) unsaved.set(this.key, { base: this.read.text, draft: this.draft })
  }

  get dirty() { return this.read !== null && this.draft !== this.read.text }
  /** Over 512 KB the host sends a cut-off text, so saving it back would lose the rest. */
  get readOnly() { return this.read?.truncated === true }

  /** Reads the file and its history again; the draft becomes what is on disk. */
  async open() {
    const request = ++this.request
    this.loading = true
    this.error = null
    this.conflict = false
    this.changed()
    try {
      const c = await this.client()
      // History first: a write between the two calls then leaves `openedNewest` older than the text, which only
      // makes a later save warn, never overwrite silently.
      const versions = await c.hqMemoryHistory(this.botId, this.shared, this.name)
      const read = await c.hqMemoryRead(this.botId, this.shared, this.name)
      if (request !== this.request) return
      const kept = unsaved.get(this.key)
      unsaved.delete(this.key)
      this.restored = !!kept && kept.base === read.text && kept.draft !== read.text && this.read === null
      this.read = read
      this.draft = this.restored ? kept!.draft : read.text
      this.versions = versions
      this.openedNewest = versions[0]?.id ?? null
      this.diffs.clear()
    } catch (error) {
      if (request === this.request) this.error = message(error)
    }
    if (request === this.request) {
      this.loading = false
      this.changed()
    }
  }

  setDraft(text: string) {
    this.draft = text
    this.changed()
  }

  /** Saves the draft. When the newest version moved since the file was opened, holds back (`conflict`) unless `force`. */
  async save(force = false) {
    if (this.busy || !this.read || this.readOnly) return
    await this.run(async (c) => {
      if (!force) {
        const newest = (await c.hqMemoryHistory(this.botId, this.shared, this.name, 1))[0]?.id ?? null
        if (movedSince(this.openedNewest, newest)) {
          this.conflict = true
          return false
        }
      }
      this.warnings = (await c.hqMemorySave(this.botId, this.shared, this.name, this.draft)).warnings
      return true
    })
  }

  async restore(version: number) {
    if (this.busy) return
    await this.run(async (c) => {
      this.warnings = (await c.hqMemoryRestore(this.botId, this.shared, this.name, version)).warnings
      return true
    })
  }

  /** Deletes the file; it is kept as a version, so the history can bring it back. Returns whether it went. */
  async delete(): Promise<boolean> {
    if (this.busy) return false
    let gone = false
    await this.run(async (c) => {
      await c.hqMemoryDelete(this.botId, this.shared, this.name)
      gone = true
      return true
    })
    return gone
  }

  /** The changes a version made, against the next older one; nothing for the oldest. */
  async showDiff(index: number) {
    const version = this.versions[index]
    if (!version || this.diffs.has(version.id)) return
    const previous = previousVersion(this.versions, index)
    this.diffs.set(version.id, 'loading')
    this.changed()
    try {
      const { diff } = await (await this.client()).hqMemoryDiff(this.botId, this.shared, this.name, previous?.id ?? null, version.id)
      this.diffs.set(version.id, { diff })
    } catch (error) {
      this.diffs.set(version.id, { error: message(error) })
    }
    this.changed()
  }

  hideDiff(id: number) {
    this.diffs.delete(id)
    this.changed()
  }

  /** Runs a write; after it the file and history are read again so what shows is what is on disk. */
  private async run(write: (c: HostClient) => Promise<boolean>) {
    this.busy = true
    this.error = null
    this.warnings = []
    this.changed()
    const warnings = () => this.warnings
    try {
      const wrote = await write(await this.client())
      if (wrote) {
        const kept = warnings()
        await this.open()
        this.warnings = kept
      }
    } catch (error) {
      this.error = message(error)
    } finally {
      this.busy = false
      this.changed()
    }
  }
}

function message(error: unknown) { return error instanceof Error ? error.message : String(error) }
