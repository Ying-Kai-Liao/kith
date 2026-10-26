import { taskLabel } from '@shared/held-run'
import { useEffect } from 'react'
import { isWorkingIn, type Bot, type Entry } from '@shared/models'
import { taskLastAt, taskOpen, taskStatus, taskTitle, type TaskStatus } from '@shared/tasks'
import { Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { HStack, Spacer } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { buildChat, relativeTime } from './chat-items'
import { ChatRow, WorkingIndicator } from './ChatRows'
import { useReading } from './reading'

const STATUS: Record<TaskStatus, { label: string; icon: string | null }> = {
  working: { label: 'Working', icon: null },
  needsYou: { label: 'Needs you', icon: 'exclamationmark.circle' },
  stopped: { label: 'Stopped', icon: 'stop.circle' },
  failed: { label: 'Failed', icon: 'stop.circle' },
  done: { label: 'Done', icon: 'checkmark.circle' },
}

/**
 * A task in the chat: inline (its first message, the thread under it, the bot at work) while it's going, a card once
 * it's idle. The card opens it; the open task folds back. An older task goes on in its thread (`openThread`).
 */
export function TaskRow({ root, bot, newest, now, choice, setChoice, openTrace, openThread }: {
  root: Entry
  bot: Bot
  newest: boolean
  now: number
  /** What the user picked on this device: open or folded; undefined follows the task. */
  choice: boolean | undefined
  setChoice: (open: boolean) => void
  openTrace: () => void
  openThread: (e: { id: string }) => void
}) {
  const store = useStore()
  const replies = store.replies(bot.id, root.id)
  const status = taskStatus(bot, root, replies)
  const open = taskOpen(status, newest, taskLastAt(root), now, choice)
  const count = root.data.thread?.count ?? 0
  const statusLabel = taskLabel(STATUS[status].label, replies)
  // An older reply can be missing when only live ones arrived: load the whole thread.
  const missing = open && replies.length < count
  // The composer goes on with this task; any other one goes on in its thread.
  const goesOn = newest && taskOpen(status, newest, taskLastAt(root), now)
  useEffect(() => {
    if (missing) void store.loadThread(bot.id, root.id)
  }, [missing, store, bot.id, root.id])

  if (!open) {
    return (
      <button className="press task-card" onClick={() => setChoice(true)} aria-label={`Open task: ${taskTitle(root)}, ${statusLabel}`}>
        <HStack gap={8}>
          <TaskStatusIcon status={status} />
          <Text size="subheadline" weight="medium" tone="text" truncate>{taskTitle(root)}</Text>
          <Spacer />
          {(root.data.thread?.unread ?? 0) > 0 ? <span className="task-unread" role="img" aria-label="Unread replies" /> : null}
          <Text size="footnote" tone="tertiary">{count === 1 ? '1 reply' : `${count} replies`} · {relativeTime.day(taskLastAt(root))}</Text>
          <Icon name="chevron.down" size={10} color="var(--tertiary)" />
        </HStack>
      </button>
    )
  }
  // A message parked at its agent's limit needs you with nothing at work: its card has the Stop.
  const working = status === 'working' || (status === 'needsYou' && isWorkingIn(bot, bot.id, root.id))
  return (
    <div className="task-open">
      <ReadingTask botId={bot.id} root={root.id} />
      <HStack gap={6} className="task-header">
        <TaskStatusIcon status={status} />
        <Text size="caption" tone="secondary">{statusLabel}</Text>
        <Spacer />
        {working ? (
          <button className="press task-action" title="Stop this task" aria-label="Stop this task" onClick={() => store.stop(bot.id, root.id)}>
            <Icon name="stop.fill" size={9} />
          </button>
        ) : null}
        {!goesOn ? (
          <button className="press task-action" title="Continue this task" aria-label="Continue this task" onClick={() => openThread(root)}>
            <Icon name="arrowshape.turn.up.left" size={10} />
          </button>
        ) : null}
        <button className="press task-action" title="Fold this task" aria-label="Fold this task" onClick={() => setChoice(false)}>
          <Icon name="chevron.up" size={10} />
        </button>
      </HStack>
      <ChatRow entry={root} groupStart chat={bot} openTrace={openTrace} />
      {buildChat(replies).map((item) =>
        item.kind === 'entry' ? <ChatRow key={item.id} entry={item.entry} groupStart={item.groupStart} chat={bot} openTrace={openTrace} /> : null,
      )}
      {working && !store.isOffline ? (
        <div className="chat-working">
          <WorkingIndicator bot={bot} thinking={store.currentThinking(bot.id, root.id)} />
        </div>
      ) : null}
    </div>
  )
}

/** While a task is open its thread is on screen: the bot's answers there are read. */
function ReadingTask({ botId, root }: { botId: string; root: string }) {
  useReading(useStore(), botId, root)
  return null
}

function TaskStatusIcon({ status }: { status: TaskStatus }) {
  const icon = STATUS[status].icon
  return icon ? <Icon name={icon} size={11} color={status === 'needsYou' ? 'var(--attention)' : 'var(--tertiary)'} /> : <Spinner size={9} />
}
