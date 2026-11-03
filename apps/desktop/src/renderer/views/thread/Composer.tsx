import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type ReactNode } from 'react'
import { isGroup, isWorkingIn, type Bot } from '@shared/models'
import { defaultFolderOf } from '@shared/place-folders'
import { currentTask, sendInTask, takesOver, taskTitle } from '@shared/tasks'
import { threadWriterMessage } from '@shared/thread-writer'
import { BotAvatar } from '../../components/Avatar'
import { Icon } from '../../components/Icon'
import { usePresence } from '../../components/Overlay'
import { VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import { useStore } from '../../store/context'
import { MAX_FILE_SIZE, type OutgoingFile } from '../../store/bot-store'
import { StartingFolderChip, type StartingPick } from '../bots/PlaceFolderPicker'
import { pluginsFor } from '../marketplace/plugins'
import { quotesOf, withQuote } from './reply-quote'
import { targetsOf, useDestination } from './task-target'
import './task-card.css'
import { composerEnter } from './composer-keyboard'
import { CommandMenu, useCommandMenu } from './CommandMenu'
import { TurnsChip, TurnsPopover } from './ConversationLine'
import { clampTurns, DEFAULT_TURNS } from '@shared/conversation'
import { ComposerAttachment } from './ComposerAttachment'
import { useDictation } from './dictation'
import { builtInCommand, slashCommands, slashQuery, type SlashCommand } from './slash-commands'

type Trailing = 'interrupt' | 'stop' | 'call' | 'send'

/**
 * The message box under a chat: send, or stop while the bot (or the group) is
 * working there. In a group, typing `@` suggests its bots; with one bot, a leading `/` lists its commands.
 */
export function Composer({ botId, onCall, onInterrupt, liveLine = false }: {
  botId: string
  /** The room's live line is up above the box and carries Stop, so the box's own doesn't. */
  liveLine?: boolean
  /** Starts a voice call; while the box is empty it takes the send button's place. */
  onCall?: (() => void) | null
  /** Interrupts the bot while it is speaking in a voice call. */
  onInterrupt?: (() => void) | null
}) {
  const store = useStore()
  // Remount transient editor state (selection, attachments, IME) when its destination changes.
  return <DestinationComposer key={JSON.stringify([store.contextId, store.computer.id, botId])}
    botId={botId} onCall={onCall} onInterrupt={onInterrupt} liveLine={liveLine} />
}

function DestinationComposer({ botId, onCall, onInterrupt, liveLine }: {
  botId: string; onCall?: (() => void) | null; onInterrupt?: (() => void) | null; liveLine: boolean
}) {
  const store = useStore()
  const plugins = useModel(pluginsFor(store))
  const bot = store.bots.get(botId) ?? null
  const drafts = store.composerDrafts
  const draft = useSyncExternalStore(drafts.subscribe, () => drafts.get(botId, null))
  const setDraft = (text: string) => drafts.set(botId, null, text)
  const [files, setFiles] = useState<OutgoingFile[]>([])
  const appendRef = useRef<(incoming: { name: string; data: Uint8Array }[]) => void>(() => {})
  const dictation = useDictation(draft, setDraft)
  const field = useRef<HTMLTextAreaElement>(null)
  const measure = useRef<HTMLDivElement>(null)
  const composing = useRef(false)
  /** Text pasted into this draft since the last send. */
  const pasted = useRef<string[]>([])
  // The chat's one box sends into a task: the one a card's Reply picked, else the newest while it goes on (as the host
  // would route it). "New task" starts another; with no task going on, the message starts one anyway.
  const tasks = !!bot && bot.tasks
  const targets = targetsOf(store)
  const { going, picked, newTask, target } = useDestination(store, botId)
  const goesOn = newTask ? null : going
  // Only a task the user picked is taken over from an idle terminal; the pill says so before the send.
  const takeOver = !!goesOn && takesOver(goesOn.data.thread, picked)
  const startsNew = tasks && newTask
  const place = tasks ? goesOn?.id ?? currentTask(store.chat(botId))?.id ?? null : null
  const [taking, setTaking] = useState(false)
  // "Reply" on a message: its first line, quoted over the box and in front of what is sent.
  const quotes = quotesOf(store)
  useSyncExternalStore(quotes.subscribe, quotes.getVersion)
  const quote = quotes.get(botId)
  // The folder the next new task starts in; the chat's own shows until one is picked.
  const [taskFolder, setTaskFolder] = useState<StartingPick | null>(null)
  // A task's Stop is on its card; a turn in the chat's own lane has no card, so its Stop stays here.
  const working = !!bot && (isWorkingIn(bot, botId, place) || isWorkingIn(bot, botId, null)) && !startsNew && !liveLine
  const stopHere = working && (!tasks || isWorkingIn(bot, botId, null))
  // In a group, how many turns this message gives the room; it starts at 10 for every message.
  const inGroup = !!bot && isGroup(bot)
  const [turns, setTurns] = useState(DEFAULT_TURNS)
  const [turnsOpen, setTurnsOpen] = useState(false)
  const empty = !draft.trim() && !files.length
  const canSend = !empty && !taking && (store.connection.kind === 'online' || (store.canQueue && !files.length))
  const canAttach = !!bot

  const placeholder = bot && isGroup(bot) ? 'Message everyone · @ to ask one bot' : startsNew ? `Tell ${bot?.name ?? ''}…` : working ? `Queue a message for ${bot?.name ?? ''}` : `Tell ${bot?.name ?? ''}…`

  // A routine's "Try it" fills the box.
  const routineDraft = store.routineDrafts.get(botId)
  useEffect(() => {
    if (routineDraft === undefined) return
    const incoming = store.consumeRoutineDraft(botId)
    if (incoming === undefined) return
    const previous = drafts.get(botId, null)
    drafts.set(botId, null, previous ? `${previous}\n${incoming}` : incoming)
    field.current?.focus()
  }, [routineDraft, botId, store, drafts])

  // Files picked in the sidebar's dock arrive here once their chat is open.
  const staged = store.stagedFiles.get(botId)
  useEffect(() => {
    if (!staged) return
    const incoming = store.takeStagedFiles(botId)
    if (incoming) appendRef.current(incoming)
    field.current?.focus()
  }, [staged, botId, store])

  // Measure a separate, wrapping copy: collapsing the live textarea to zero breaks its
  // height transition, caret scroll and IME composition. Observe font/width changes too.
  useLayoutEffect(() => {
    const el = field.current
    const mirror = measure.current
    if (!el || !mirror) return
    const resize = () => {
      const style = getComputedStyle(el)
      const line = parseFloat(style.lineHeight)
      const padding = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom)
      const height = Math.min(mirror.getBoundingClientRect().height, line * 8 + padding)
      el.style.height = `${height}px`
    }
    resize()
    const observer = new ResizeObserver(resize)
    observer.observe(mirror)
    return () => observer.disconnect()
  }, [])

  const mentionQuery = (() => {
    if (!bot || !isGroup(bot)) return null
    const at = draft.lastIndexOf('@')
    if (at < 0) return null
    if (at > 0 && !/\s/.test(draft[at - 1]!)) return null
    const query = draft.slice(at + 1)
    return /\n/.test(query) || query.length > 24 ? null : query
  })()
  const suggestions: Bot[] =
    mentionQuery !== null && bot ? store.members(bot).filter((m) => !mentionQuery || m.name.toLowerCase().includes(mentionQuery.toLowerCase())) : []

  const mention = (member: Bot) => {
    const at = draft.lastIndexOf('@')
    if (at < 0) return
    setDraft(`${draft.slice(0, at)}@${member.name} `)
    field.current?.focus()
  }

  // Not in a group: each member has its own skills.
  const skillsShown = !!bot && !isGroup(bot)
  const slash = slashQuery(draft)
  const commands = slash === null ? [] : slashCommands(bot, slash, skillsShown ? plugins.installedSkills : [])

  const run = (command: SlashCommand) => {
    if (!bot) return
    switch (command.action.kind) {
      case 'fast':
        store.setFastMode(bot, !bot.fastMode)
        setDraft('')
        break
      case 'skill':
        setDraft(`/${command.action.id} `)
        break
    }
    field.current?.focus()
  }
  const menu = useCommandMenu(draft, commands, run)

  const append = (incoming: { name: string; data: Uint8Array }[]) => {
    if (!canAttach) return
    const accepted: OutgoingFile[] = []
    for (const f of incoming) {
      if (f.data.length > MAX_FILE_SIZE) {
        store.setError(`${f.name} is larger than 100 MB.`)
        continue
      }
      accepted.push({ id: crypto.randomUUID(), name: f.name, data: f.data })
    }
    setFiles((list) => [...list, ...accepted])
  }

  appendRef.current = append

  const submit = () => {
    // Send while recording: the recording ends and what it heard goes with the message.
    if (dictation.listening) dictation.stop()
    const command = files.length ? null : builtInCommand(bot, draft)
    if (command) return run(command)
    if (!canSend) return
    const into = goesOn
    const text = withQuote(draft, quote)
    const send = () => store.send(text, botId, into?.id ?? null, files, undefined, pasted.current, startsNew, newTask ? (taskFolder?.start ?? null) : null, null, inGroup ? clampTurns(turns) : null)
    // A picked task an idle terminal holds is taken over first; if that fails the text stays here, never sent as a new task.
    setTaking(true)
    void sendInTask(into?.data.thread, picked, () => store.ready().then((c) => c.takeOverThread(botId, into!.id)), send).then((failure) => {
      setTaking(false)
      if (failure === null) return clear()
      store.setError(`Couldn't take over from the terminal, so nothing was sent (${threadWriterMessage(failure)}). Your message is still in the box.`)
    })
  }

  const clear = () => {
    setTurns(DEFAULT_TURNS)
    setTurnsOpen(false)
    pasted.current = []
    quotes.set(botId, null)
    if (newTask) targets.set(botId, null)
    setTaskFolder(null)
    setDraft('')
    setFiles([])
    field.current?.focus()
  }

  // While recording, the mic is its Stop and this is its Send.
  const trailing: Trailing = dictation.listening ? 'send' : onInterrupt && !draft && !files.length ? 'interrupt' : stopHere && !draft && !files.length ? 'stop' : onCall && !draft && !files.length ? 'call' : 'send'
  const [symbol, label] = ({
    interrupt: ['stop.fill', 'Interrupt'],
    stop: ['stop.fill', 'Stop'],
    call: ['waveform', 'Call'],
    send: ['arrow.up', 'Send'],
  } as const)[trailing]
  const enabled = trailing !== 'send' || canSend

  return (
    <VStack>
      <ComposerReveal open={suggestions.length > 0}>
        <div className="mentions">
          {suggestions.map((m) => (
            <button key={m.id} className="press mention" onClick={() => mention(m)}>
              <BotAvatar bot={m} size={18} animated={false} />
              <Text size="subheadline" tone="text">{m.name}</Text>
            </button>
          ))}
        </div>
      </ComposerReveal>
      <ComposerReveal open={tasks && (!empty || !!target)}>
        <div className="task-pills" role="radiogroup" aria-label="Where this message goes">
          {going ? (
            <button className={`press task-pill ${newTask ? '' : 'on'}`} role="radio" aria-checked={!newTask} onClick={() => { targets.set(botId, going.id); field.current?.focus() }}>
              <span aria-hidden>↳</span>
              Continues <b>{taskTitle(going)}</b>
              {takeOver && !newTask ? <span className="task-pill-note">· takes over from the terminal</span> : null}
            </button>
          ) : null}
          <button className={`press task-pill ${newTask || !goesOn ? 'on' : ''}`} role="radio" aria-checked={newTask || !goesOn} onClick={() => { targets.set(botId, 'new'); field.current?.focus() }}>
            <Icon name="plus" size={10} weight="semibold" />
            New task
          </button>
        </div>
      </ComposerReveal>
      <div
        className="composer"
        onDragOver={(e) => {
          if (canAttach && e.dataTransfer.types.includes('Files')) e.preventDefault()
        }}
        onDrop={(e) => {
          if (!canAttach) return
          e.preventDefault()
          void Promise.all([...e.dataTransfer.files].map(async (f) => ({ name: f.name, data: new Uint8Array(await f.arrayBuffer()) }))).then(append)
        }}
      >
        <div className="composer-surface">
          {inGroup && !empty && turnsOpen ? <TurnsPopover turns={turns} setTurns={setTurns} close={() => { setTurnsOpen(false); field.current?.focus() }} /> : null}
          <CommandMenu commands={menu.shown} index={menu.index} hover={menu.hover} pick={run} />
          <ComposerReveal open={!!quote}>
            <div className="reply-chip">
              <Icon name="arrowshape.turn.up.left" size={11} color="var(--secondary)" />
              <Text size="footnote" tone="secondary" truncate>{quote ?? ''}</Text>
              <button className="press reply-chip-x" title="Don't reply to this" aria-label="Remove the quoted message" onClick={() => { quotes.set(botId, null); field.current?.focus() }}>
                <Icon name="xmark" size={9} weight="semibold" />
              </button>
            </div>
          </ComposerReveal>
          <ComposerReveal open={newTask && !!bot?.folder}>
            <div className="place-chip-row composer-folder">
              <StartingFolderChip inherited={bot ? defaultFolderOf(bot) : null} value={taskFolder} onChange={setTaskFolder} />
            </div>
          </ComposerReveal>
          <ComposerReveal open={files.length > 0}>
            <div className="file-chips">
              {files.map((f) => (
                <ComposerAttachment key={f.id} file={f} remove={() => setFiles((list) => list.filter((x) => x.id !== f.id))} />
              ))}
            </div>
          </ComposerReveal>
          <div className="composer-box">
            {/* One capsule: attachments, the field, and send / stop / call. */}
            {canAttach ? (
              <button className="press composer-add" aria-label="Add files" title="Add files" onClick={() => void window.kith.app.pickFiles().then(append)}>
                <Icon name="plus" size={13} weight="medium" color="var(--text)" />
              </button>
            ) : null}
            <div className="composer-field">
              <div ref={measure} className="composer-measure" aria-hidden="true">{draft + '\u200b'}</div>
              <textarea
                ref={field}
                rows={1}
                value={draft}
                readOnly={dictation.listening}
                placeholder={placeholder}
                spellCheck
                aria-label="Message"
                aria-keyshortcuts="Enter Shift+Enter"
                onCompositionStart={() => { composing.current = true }}
                onCompositionEnd={() => { composing.current = false }}
                onChange={(e) => {
                  setDraft(e.target.value)
                  // Starting a command: list the computer's skills again.
                  if (skillsShown && e.target.value === '/') void plugins.refresh()
                }}
                onKeyDown={(e) => {
                  // With the turns popover open, Up and Down step the number; Return still sends.
                  if (turnsOpen && (e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
                    e.preventDefault()
                    setTurns((n) => clampTurns(n + (e.key === 'ArrowUp' ? 1 : -1)))
                    return
                  }
                  if (menu.onKey(e, composing.current || e.nativeEvent.isComposing || e.keyCode === 229)) return
                  // Return sends, Shift-Return adds a line; an input method's Return picks its candidate.
                  const action = composerEnter({ key: e.key, shiftKey: e.shiftKey, altKey: e.altKey, repeat: e.repeat, isComposing: composing.current || e.nativeEvent.isComposing, keyCode: e.keyCode })
                  if (e.key === 'Enter' && e.repeat && !e.shiftKey && !e.altKey) e.preventDefault()
                  if (action === 'send') {
                    e.preventDefault()
                    submit()
                  }
                }}
                onPaste={(e) => {
                  if (dictation.listening) return
                  const types = [...e.clipboardData.types]
                  const hasFiles = types.includes('Files')
                  // Pasted text is sent marked as pasted: memory never takes it as the user's own words.
                  if (types.includes('text/plain') && !e.clipboardData.files.length) pasted.current.push(e.clipboardData.getData('text/plain'))
                  // ⌘V with a copied file or image: attach it (the field alone would paste a name).
                  if (!canAttach) return
                  if (!hasFiles || (types.includes('text/plain') && !e.clipboardData.files.length)) return
                  e.preventDefault()
                  void window.kith.app.readClipboardFiles().then((picked) => {
                    if (picked.length) return append(picked)
                    return Promise.all([...e.clipboardData.files].map(async (f) => ({ name: f.name || `pasted-${Date.now()}.png`, data: new Uint8Array(await f.arrayBuffer()) }))).then(append)
                  })
                }}
              />
            </div>
            {dictation.available ? (
              <button
                className={`press composer-mic ${dictation.listening ? 'on' : ''}`}
                aria-label={dictation.listening ? 'Stop recording' : 'Dictate'}
                title={dictation.listening ? 'Stop recording: edit before sending' : 'Dictate'}
                onClick={() => {
                  if (!dictation.listening) return dictation.start()
                  dictation.stop()
                  field.current?.focus()
                }}
              >
                <Icon name={dictation.listening ? 'stop.fill' : 'mic'} size={dictation.listening ? 11 : 13} weight="medium" />
              </button>
            ) : null}
            {inGroup && !empty ? <TurnsChip turns={turns} open={turnsOpen} toggle={() => setTurnsOpen((o) => !o)} /> : null}
            <button
              className={trailing === 'interrupt' ? 'press composer-send interrupt' : 'press composer-send'}
              disabled={!enabled}
              aria-label={label}
              title={label}
              onClick={() => {
                if (trailing === 'interrupt') onInterrupt?.()
                else if (trailing === 'stop') store.stop(botId, tasks ? null : place)
                else if (trailing === 'call') onCall?.()
                else submit()
              }}
            >
              {(['stop.fill', 'waveform', 'arrow.up'] as const).map((name) => (
                <span key={name} aria-hidden="true" className={`composer-symbol ${name === symbol ? 'active' : ''}`}>
                  <Icon name={name} size={name === 'stop.fill' ? 12 : name === 'waveform' ? 15 : 14} weight="bold" />
                </span>
              ))}
            </button>
          </div>
        </div>
      </div>
    </VStack>
  )
}

/** Keep the outgoing row until its height/fade transition finishes. */
function ComposerReveal({ open, children }: { open: boolean; children: ReactNode }) {
  const { mounted, shown } = usePresence(open)
  const last = useRef(children)
  if (open) last.current = children
  return <div className={`composer-reveal ${shown ? 'shown' : ''}`} inert={!open}>
    <div>{mounted ? (open ? children : last.current) : null}</div>
  </div>
}
