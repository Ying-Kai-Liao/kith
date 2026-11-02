import { useEffect, useMemo, useRef, useState } from 'react'
import { diffLines, authorText, causeText, changeText, fileOrder, type HqMemoryFile, type HqTidyUp } from '@shared/hq-memory'
import { formatBytes } from '@shared/kith-info'
import { Button, CardSection, Spinner, TabChoice } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { Dialog, ModalHeader, Sheet } from '../../components/Overlay'
import { useModel } from '../../lib/observable'
import { useStore } from '../../store/context'
import { HqFileEditor, HqMemoryStore } from '../../store/hq-memory-store'
import { relativeTime } from '../thread/chat-items'
import { AgentMemorySearch } from './kith/KithMemory'
import './memory.css'

type Section = 'yours' | 'agent' | 'tidy'
interface Target { shared: boolean; name: string }

const SECTIONS: { id: Section; label: string }[] = [
  { id: 'yours', label: 'About you' }, { id: 'agent', label: 'This agent' }, { id: 'tidy', label: 'Tidy-ups' },
]

/** An hq agent's memory, the one memory it has: its plain files. About you (shared files), This agent (its own) and Tidy-ups. */
export function HqMemory({ botId }: { botId: string }) {
  const host = useStore()
  const store = useModel(useMemo(() => new HqMemoryStore(botId, () => host.ready()), [botId, host]))
  const bot = host.bots.get(botId)
  const [section, setSection] = useState<Section>('agent')
  const [target, setTarget] = useState<Target | null>(null)
  // The open file asks before it closes with an unsaved edit; Esc and the header's close go through it too.
  const guard = useRef<() => void>(() => undefined)
  // The host emits the bot when a file changes (an agent's write, a tidy-up), which moves `memoryRev` on some hosts.
  useEffect(() => { void store.load() }, [store, bot?.memoryRev])

  const files = store.list ? (section === 'yours' ? store.list.shared : store.list.agent) : []
  // The index file can be written before it exists.
  const index = section === 'yours' ? 'user.md' : 'MEMORY.md'
  const rows: HqMemoryFile[] = fileOrder(files.some((f) => f.name === index) ? files : [...files, { name: index, shared: section === 'yours', path: index, size: 0, modified: null }])

  return <>
    <div className="memory-panel">
      <TabChoice label="Memory" selection={section} options={SECTIONS} onChange={(next) => {
        setSection(next)
        // A tidy-up or an agent's write lands while the tab is open, with nothing to move `memoryRev`: read again on the way in.
        void store.load()
      }} />
      <CardSection>
        <div className="memory-stack">
        {store.error && <p role="alert" className="memory-error">{store.error}</p>}
        {!store.list && !store.error && <p className="memory-muted" role="status">Loading memory…</p>}
        {store.list && section === 'yours' && <>
          <p className="memory-muted">Your name, people and projects, shared by your agents. Plain files you can edit.</p>
          {rows.map((f) => <FileRow key={f.name} file={f} open={() => setTarget({ shared: true, name: f.name })} />)}
        </>}
        {store.list && section === 'agent' && <>
          <p className="memory-muted">What this agent keeps in its own files. Plain files you can edit.</p>
          <ForgetFact store={store} />
          <AgentMemorySearch botId={botId} target={(hit) => searchTarget(hit.path, store.list?.shared ?? [])} open={setTarget}>
            {rows.map((f) => <FileRow key={f.name} file={f} open={() => setTarget({ shared: false, name: f.name })} />)}
          </AgentMemorySearch>
        </>}
        {store.list && section === 'tidy' && <TidyUps store={store} />}
        </div>
      </CardSection>
    </div>
    <Sheet open={target !== null} onClose={() => guard.current()} width={640}>
      {target && <FileSheet key={`${target.shared}:${target.name}`} botId={botId} store={store} target={target} guard={guard} onClose={() => setTarget(null)} />}
    </Sheet>
  </>
}

/** What a search hit opens: a shared file (the host gives its absolute path), or a Markdown file of the agent's folder. */
function searchTarget(path: string, shared: HqMemoryFile[]): Target | null {
  const user = shared.find((f) => f.path === path)
  if (user) return { shared: true, name: user.name }
  return /\.md$/i.test(path) && !path.startsWith('/') ? { shared: false, name: path } : null
}

/** "Forget a fact": kith keeps it out of every agent's memory from now on. Lines already written stay until edited. */
function ForgetFact({ store }: { store: HqMemoryStore }) {
  const [text, setText] = useState('')
  const submit = () => { void store.forget(text).then((ok) => { if (ok) setText('') }) }
  return <div className="memory-forget">
    <input className="memory-forget-input" aria-label="Forget a fact" placeholder="Forget a fact, e.g. lives in Osaka" spellCheck={false}
      value={text} disabled={store.busy} onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') submit() }} />
    <Button kind="secondary" disabled={store.busy || !text.trim()} onClick={submit}>Forget</Button>
    {store.forgetError && <p role="alert" className="memory-error">{store.forgetError}</p>}
    {store.forgot && <p className="memory-muted" role="status">No agent will write “{store.forgot}” to memory again. Lines already written stay until you edit them.</p>}
  </div>
}

function FileRow({ file, open }: { file: HqMemoryFile; open: () => void }) {
  const made = file.modified !== null || file.size > 0
  return <button className="press memory-content fade-in" onClick={open}>
    <strong><Icon name="doc.text" size={11} />{file.name}</strong>
    <small>{made ? `${formatBytes(file.size)}${file.modified ? ` · ${relativeTime.day(file.modified)}` : ''}` : 'Not made yet'}</small>
  </button>
}

function TidyUps({ store }: { store: HqMemoryStore }) {
  const [confirm, setConfirm] = useState<HqTidyUp | null>(null)
  return <>
    <p className="memory-muted">Between chats the agent tidies its memory from your own words. Each run can be undone.</p>
    {store.undoError && <p role="alert" className="memory-error">{store.undoError}</p>}
    {store.tidyError && <p role="alert" className="memory-error">Couldn't read the tidy-ups: {store.tidyError}</p>}
    {store.tidyLoaded && !store.tidyUps.length && <div className="memory-empty">
      <Icon name="sparkles" size={20} color="var(--tertiary)" />
      <span>Tidy-ups appear after you chat with this agent and it has been idle for a few minutes.</span>
    </div>}
    {store.tidyUps.map((run) => {
      const result = store.undone?.id === run.id ? store.undone.result : null
      return <div className="memory-item fade-in" key={run.id}>
        <div className="memory-content">
          <strong>{run.at ? relativeTime.day(run.at) : 'Tidy-up'}{run.undone ? ' · undone' : ''}</strong>
          {run.changes.map((c, i) => <span key={i} className={c.kind === 'archived' ? 'memory-archived' : undefined}>{changeText(c)}</span>)}
          {!run.changes.length && run.files.map((f) => <span key={`${f.shared}:${f.name}`} className="memory-muted">Changed {f.name}</span>)}
          {result?.refused.map((r) => <span key={r.file} className="memory-attention">Left alone: {r.file}. {r.reason}</span>)}
          {result && !result.undone && result.restored.length > 0 && <span className="memory-muted">Put back {result.restored.join(', ')}.</span>}
        </div>
        {!run.undone && <Button kind="secondary" disabled={store.busy} onClick={() => setConfirm(run)}>Undo this tidy</Button>}
      </div>
    })}
    <Dialog open={confirm !== null} title="Undo this tidy?"
      message="Files it changed go back to what they were. A file changed since is left alone."
      actions={[{ title: 'Undo this tidy', destructive: true, action: () => { if (confirm) void store.undo(confirm.id) } }]}
      onClose={() => setConfirm(null)} />
  </>
}

/** One file: edit and save it, or read its history, compare versions and bring one back. */
function FileSheet({ botId, store, target, guard, onClose }: { botId: string; store: HqMemoryStore; target: Target; guard: { current: () => void }; onClose: () => void }) {
  const host = useStore()
  const editor = useModel(useMemo(() => new HqFileEditor(botId, target.shared, target.name, () => host.ready()), [botId, host, target]))
  const [tab, setTab] = useState<'edit' | 'history'>('edit')
  const [discard, setDiscard] = useState(false)
  const [removing, setRemoving] = useState(false)
  // `undo`: put back the file as it was before this version's change (the next older one).
  const [restore, setRestore] = useState<{ id: number; undo: boolean } | null>(null)
  useEffect(() => { void editor.open() }, [editor])
  // Leaving the tab or switching bot with an edit open keeps it for when the file is opened again.
  useEffect(() => () => editor.stash(), [editor])

  // Leaving with an unsaved edit asks first.
  const close = () => { if (editor.dirty) setDiscard(true); else onClose() }
  guard.current = close
  const refreshList = () => { void store.load() }
  const save = (force = false) => { void editor.save(force).then(refreshList) }

  const read = editor.read
  const botName = (id: string | null) => (id ? host.authorName(id) : 'The agent')
  return <div className="memory-file-sheet">
    <ModalHeader title={`${target.shared ? 'Shared · ' : ''}${target.name}`} onClose={close}
      trailing={editor.busy ? <Spinner /> : <Button disabled={!editor.dirty || editor.readOnly} onClick={() => save()}>Save</Button>} />
    <div className="memory-sheet-body">
      <TabChoice label="File" selection={tab} options={[{ id: 'edit', label: 'Edit' }, { id: 'history', label: 'History' }]} onChange={setTab} />
      {editor.error && <p role="alert" className="memory-error">{editor.error}</p>}
      {editor.loading && !read && <p className="memory-muted" role="status"><Spinner size={12} /> Loading…</p>}
      {editor.restored && <p className="memory-attention" role="status">Your unsaved edit from before is back.</p>}
      {editor.warnings.map((w) => <p key={w} className="memory-attention" role="status">{w}</p>)}
      {editor.conflict && <div className="memory-conflict" role="alert">
        <p>This file changed since you opened it. Saving now would replace that change.</p>
        <div className="memory-header-actions">
          <Button kind="secondary" disabled={editor.busy} onClick={() => void editor.open()}>Reload (drops your edit)</Button>
          <Button disabled={editor.busy} onClick={() => save(true)}>Save anyway</Button>
        </div>
      </div>}
      {read && tab === 'edit' && <>
        {read.exists && !(!target.shared && target.name === 'MEMORY.md') && <div className="memory-header-actions">
          <Button kind="secondary" disabled={editor.busy} onClick={() => setRemoving(true)}>Delete file</Button>
        </div>}
        {read.truncated && <p className="memory-muted">Longer than 512 KB: showing the start, read-only here. Edit this file in an editor.</p>}
        {!read.exists && <p className="memory-muted">Not made yet. Saving creates it.</p>}
        <textarea className="memory-file-edit" aria-label={`Contents of ${target.name}`} spellCheck={false} readOnly={editor.readOnly || editor.busy}
          value={editor.draft} onChange={(e) => editor.setDraft(e.target.value)} />
      </>}
      {read && tab === 'history' && <div className="memory-versions">
        {editor.versions.length > 1 && <p className="memory-muted">Each row is the file after that change. Restore this version brings that state back; Undo this change brings back the state before it.</p>}
        {!editor.versions.length && <p className="memory-muted">No versions yet. The first one is kept when the file is first changed.</p>}
        {editor.versions.map((v, i) => {
          const diff = editor.diffs.get(v.id)
          const older = editor.versions[i + 1]
          return <div key={v.id} className="memory-version">
            <div className="memory-item">
              <div className="memory-content">
                <strong>{v.cause === 'baseline' ? causeText(v.cause) : `${authorText(v, botName)} · ${causeText(v.cause)}`}</strong>
                <small>{v.at ? relativeTime.day(v.at) : ''}{v.deleted ? ' · deleted' : ` · ${v.lines} ${v.lines === 1 ? 'line' : 'lines'}`}{i === 0 ? ' · newest' : ''}</small>
              </div>
              {older && <Button kind="secondary" onClick={() => (diff ? editor.hideDiff(v.id) : void editor.showDiff(i))}>{diff ? 'Hide changes' : 'Show changes'}</Button>}
            </div>
            {(i > 0 && !v.deleted) || (older && !older.deleted) ? <div className="memory-item memory-version-actions">
              {i > 0 && !v.deleted && <Button kind="secondary" disabled={editor.busy} onClick={() => setRestore({ id: v.id, undo: false })}>Restore this version</Button>}
              {older && !older.deleted && <Button kind="secondary" disabled={editor.busy} onClick={() => setRestore({ id: older.id, undo: true })}>Undo this change</Button>}
            </div> : null}
            {diff === 'loading' && <p className="memory-muted" role="status"><Spinner size={12} /> Loading…</p>}
            {diff && diff !== 'loading' && ('error' in diff
              ? <p role="alert" className="memory-error">{diff.error}</p>
              : <pre className="memory-diff">{diffLines(diff.diff).length
                ? diffLines(diff.diff).map((l, n) => <span key={n} className={`memory-diff-${l.kind}`}>{l.kind === 'add' ? '+ ' : l.kind === 'del' ? '- ' : '  '}{l.text}{'\n'}</span>)
                : 'No changes.'}</pre>)}
          </div>
        })}
      </div>}
    </div>
    <Dialog open={discard} title="Discard your changes?" message="You have edits that are not saved."
      actions={[{ title: 'Discard', destructive: true, action: onClose }]} onClose={() => setDiscard(false)} />
    <Dialog open={removing} title={`Delete ${target.name}?`}
      message="The agent stops seeing it. The deletion is kept as a version, so History can bring it back."
      actions={[{ title: 'Delete', destructive: true, action: () => { void editor.delete().then((gone) => { refreshList(); if (gone) onClose() }) } }]}
      onClose={() => setRemoving(false)} />
    <Dialog open={restore !== null} title={restore?.undo ? 'Undo this change?' : 'Restore this version?'}
      message={`${restore?.undo ? `${target.name} goes back to how it was before this change.` : `${target.name} becomes the file as it was after this change.`} It is kept as a new version.${editor.dirty ? ' Your unsaved edit is lost.' : ''}`}
      actions={[{ title: restore?.undo ? 'Undo this change' : 'Restore this version', action: () => { if (restore) void editor.restore(restore.id).then(refreshList) } }]}
      onClose={() => setRestore(null)} />
  </div>
}
