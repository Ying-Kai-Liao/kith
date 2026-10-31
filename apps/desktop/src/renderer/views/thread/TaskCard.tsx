import { useLayoutEffect, useState, type ReactNode } from 'react'
import type { Entry } from '@shared/models'
import { foldLine, taskFold, taskHead, taskOpen, taskStatus } from '@shared/tasks'
import { isDeliveryTool } from '@shared/thread-feed'
import { isBusyInTerminal, isHeldByTerminal, threadWriterMessage } from '@shared/thread-writer'
import { Icon } from '../../components/Icon'
import { LiveDot, pairOf, TakeOverDialog, WriterBanner } from '../../components/kith'
import { pairStyle } from '../../components/kith/pair'
import { useStore } from '../../store/context'
import { relativeTime } from './chat-items'
import { OpenCard, WorkingIndicator } from './ChatRows'
import type { ChatItem } from './flat-chat'
import { targetsOf } from './task-target'
import './task-card.css'

type TaskItem = Extract<ChatItem, { kind: 'task' }>

const row = (entry: Entry): ChatItem => ({ id: entry.id, kind: 'entry', entry, groupStart: true })
const isStep = (e: Entry) => !isDeliveryTool(e)

/**
 * A task in the flat chat: its first message and its thread as one card. While it works the card is open, with a
 * spine, every row and the live line (Stop sits there); once it stops it folds to the first message, a line saying
 * what the fold hides, and the last reply. A question that waits on you shows on the card, answerable there, and is
 * the only card with a fill. Reply points the chat's one box at this task.
 */
export function TaskCard({ item, renderRow, focusId }: { item: TaskItem; renderRow: (item: ChatItem) => ReactNode; focusId?: string | null }) {
  const store = useStore()
  const { root, thread } = item
  const bot = store.bots.get(root.botId)
  const status = bot ? taskStatus(bot, root, thread) : 'done'
  const [choice, setChoice] = useState<boolean | undefined>(undefined)
  const holdsFocus = !!focusId && thread.some((e) => e.id === focusId)
  useLayoutEffect(() => {
    if (holdsFocus) setChoice(true)
  }, [holdsFocus, focusId])
  const open = taskOpen(status, choice)
  const head = taskHead(status, root.data.thread, store.hostName)
  const fold = taskFold(thread, isStep)
  const line = foldLine(fold.messages, fold.steps)
  const waiting = thread.filter((e) => e.kind === 'permission' && e.data.status === 'pending')
  const asker = waiting[0] ? store.bots.get(waiting[0].data.author ?? waiting[0].botId) : undefined
  const pair = pairOf(bot?.avatarColor)
  const needsYou = status === 'needsYou'
  const working = status === 'working' && !isBusyInTerminal(root.data.thread)
  const guest = !!bot?.botGuest
  const reply = () => {
    targetsOf(store).set(root.botId, root.id)
    document.querySelector<HTMLTextAreaElement>('.composer textarea')?.focus()
  }
  const toggle = () => setChoice(!open)
  const className = ['task-card', open ? 'open' : '', needsYou ? 'needs-you' : '', status === 'working' ? 'live' : ''].filter(Boolean).join(' ')
  return (
    <div className={className} data-task={root.id} style={needsYou ? pairStyle(pairOf(asker?.avatarColor ?? bot?.avatarColor)) : undefined}>
      <button className="task-head press" onClick={toggle} aria-expanded={open}>
        {head.held ? (
          <span className="task-chip">
            {isBusyInTerminal(root.data.thread) ? <LiveDot pair={pair} /> : <Icon name="terminal" size={10} weight="medium" />}
            {head.origin}
          </span>
        ) : null}
        {head.status ? (
          <span className="task-status">
            {status === 'working' ? <LiveDot pair={pair} /> : needsYou ? <span className="task-dot" aria-hidden /> : null}
            {head.status}
          </span>
        ) : null}
        {!head.held && head.origin ? <span className="task-origin">{head.origin}</span> : null}
        <span className="task-time">{relativeTime.time(root.createdAt)}</span>
        <span className="task-head-end">
          <span className={`disclosure-chevron${open ? ' open' : ''}`}><Icon name="chevron.down" size={10} weight="semibold" /></span>
        </span>
      </button>
      <div className="task-rows">
        <div data-item={root.id}>{renderRow(row(root))}</div>
        {open ? (
          item.inner.map((r) => <div key={r.id} data-item={r.id}>{renderRow(r)}</div>)
        ) : (
          <>
            {line ? (
              <button className="task-fold press" onClick={toggle}>
                {line}
                <Icon name="chevron.down" size={9} weight="semibold" />
              </button>
            ) : null}
            {fold.last ? <div data-item={fold.last.id}>{renderRow(row(fold.last))}</div> : null}
            {waiting.map((e) => <div key={e.id} className="task-needs"><OpenCard entry={e} /></div>)}
          </>
        )}
        {open && working && bot ? (
          <div className="task-live">
            <WorkingIndicator bot={bot} thinking={store.currentThinking(root.botId, root.id)} />
            <button className="task-stop press" onClick={() => store.stop(root.botId, root.id)} aria-label="Stop this task">
              <Icon name="stop.fill" size={9} />
              Stop
            </button>
          </div>
        ) : null}
        {head.held && isHeldByTerminal(root.data.thread) && bot ? <TaskWriter root={root} agent={bot.name} /> : null}
        {!guest && !working && !head.held ? (
          <button className="task-reply press" onClick={reply}>
            <Icon name="arrowshape.turn.up.left" size={10} />
            Reply
          </button>
        ) : null}
      </div>
    </div>
  )
}

/** A task a terminal holds: who has it, and Take over, inside the card. */
function TaskWriter({ root, agent }: { root: Entry; agent: string }) {
  const store = useStore()
  const bot = store.bots.get(root.botId)
  const [phase, setPhase] = useState<'idle' | 'taking' | 'failed'>('idle')
  const [confirming, setConfirming] = useState(false)
  const busy = isBusyInTerminal(root.data.thread)
  const take = async () => {
    setPhase('taking')
    try {
      await (await store.ready()).takeOverThread(root.botId, root.id)
      setConfirming(false)
      setPhase('idle')
      targetsOf(store).set(root.botId, root.id)
    } catch (e) {
      setConfirming(false)
      setPhase('failed')
      if (e instanceof Error) store.setError(threadWriterMessage(e.message))
    }
  }
  return (
    <div className="task-writer">
      <WriterBanner
        phase={busy && phase === 'idle' ? 'working' : phase}
        mac={store.hostName}
        pair={pairOf(bot?.avatarColor)}
        mine={store.isOwner}
        pending={phase === 'taking'}
        onTakeOver={() => setConfirming(true)}
        onRetry={() => void take()}
      />
      <TakeOverDialog open={confirming} mac={store.hostName} agent={agent} resumeId={root.id} pending={phase === 'taking'} onConfirm={() => void take()} onCancel={() => setConfirming(false)} />
    </div>
  )
}
