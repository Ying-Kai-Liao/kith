import { useContext, useEffect, useRef, useState } from 'react'
import { isOpen, joinBlocked, limitParts, sendBlocked, stateLabel, stopBlocked, type Peek } from '@shared/handoff-peek'
import { Button, ProgressBar, Spinner } from '../../components/Controls'
import { Dialog, ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { AppContext } from '../../store/context'
import type { BotStore } from '../../store/bot-store'
import { Field } from '../marketplace/pieces'
import './handoff-sheet.css'

/** How often an open sheet reads the handoff again. */
const REFRESH_MS = 4000

const clock = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })

/**
 * The owner's peek at a handoff: its brief, the worker's live line and the limits it runs under, with Join (open the
 * worker's thread), Stop and a box to the worker. It reads while it is open and stops reading when it closes. Owner
 * only: the callers don't offer it to a member, and a member's host would refuse it anyway.
 */
export function HandoffSheet({ store, id, onClose }: { store: BotStore; id: string | null; onClose: () => void }) {
  return (
    <Sheet open={id !== null} onClose={onClose} width={560} dismissOnDim>
      {id !== null && <Peeking store={store} id={id} onClose={onClose} />}
    </Sheet>
  )
}

function Peeking({ store, id, onClose }: { store: BotStore; id: string; onClose: () => void }) {
  const app = useContext(AppContext)
  const [peek, setPeek] = useState<Peek | null | undefined>(undefined)
  const [confirming, setConfirming] = useState(false)
  const [stopped, setStopped] = useState(false)
  const [draft, setDraft] = useState('')
  const [sent, setSent] = useState(false)
  const alive = useRef(true)

  useEffect(() => {
    alive.current = true
    const read = async () => {
      const next = await store.peekHandoff(id)
      if (alive.current) setPeek(next)
    }
    void read()
    const timer = setInterval(() => void read(), REFRESH_MS)
    return () => {
      alive.current = false
      clearInterval(timer)
    }
  }, [store, id])

  if (peek === undefined) {
    return <SheetPage><ModalHeader title="Handoff" onClose={onClose} /><div className="hs-body"><Spinner /></div></SheetPage>
  }
  if (peek === null) {
    return (
      <SheetPage>
        <ModalHeader title="Handoff" onClose={onClose} />
        <div className="hs-body"><Text tone="secondary">There is no record of this handoff. It may be from before handoffs were kept.</Text></div>
      </SheetPage>
    )
  }

  const thread = peek.thread
  const join = joinBlocked(peek)
  const stop = stopBlocked(peek)
  const box = sendBlocked(peek)
  const from = peek.from.agent ?? peek.from.person
  const parts = limitParts(peek.limits)
  const budget = parts.find((p) => p.kind === 'budget')
  const rest = parts.filter((p) => p !== budget)

  const doJoin = () => {
    if (!thread) return
    app?.select({ computerId: store.computer.id, botId: thread.botId })
    store.requestThread(thread.botId, thread.threadId)
    onClose()
  }
  // One Stop only: a second tap while the first is settling sends nothing.
  const doStop = () => {
    if (!thread || stopped) return
    setStopped(true)
    store.stop(thread.botId, thread.threadId)
  }
  const doSend = () => {
    const text = draft.trim()
    if (!thread || !text) return
    store.send(text, thread.botId, thread.threadId)
    setDraft('')
    setSent(true)
  }

  return (
    <SheetPage>
      <ModalHeader title={from ? `${from} → ${peek.to}` : `To ${peek.to}`} subtitle={stopped && isOpen(peek.state) ? 'Stopping' : stateLabel(peek.state)} onClose={onClose} />
      <div className="hs-body">
        {peek.brief ? <pre className="hs-brief selectable">{peek.brief}</pre> : null}
        {peek.state === 'needsYou' ? <Text size="footnote" tone="secondary">It is waiting on you. The question is a card in its room.</Text> : null}
        {peek.live ? (
          <div className="hs-live">
            <span className="hs-label">{isOpen(peek.state) ? 'NOW' : 'LAST'}</span>
            <span className={`hs-live-text ${peek.live.kind === 'tool' ? 'mono' : ''}`}>{peek.live.text}</span>
          </div>
        ) : null}
        {parts.length ? (
          <div className="hs-limits" aria-label="Limits">
            {budget ? (
              <div className={`hs-limit ${budget.watch ? 'watch' : ''}`}>
                {budget.fraction !== null ? <ProgressBar value={budget.fraction} label="Budget used" /> : null}
                <span>{budget.label}</span>
              </div>
            ) : null}
            {rest.map((p) => <span key={p.kind} className={`hs-limit ${p.watch ? 'watch' : ''}`}>{p.label}</span>)}
          </div>
        ) : null}
        <div className="hs-times">
          <span>Started {clock(peek.startedMs)}</span>
          {peek.answeredMs ? <span>{peek.state === 'done' ? 'Done' : 'Ended'} {clock(peek.answeredMs)}</span> : null}
          {peek.cappedMs ? <span>Capped {clock(peek.cappedMs)}</span> : null}
        </div>
        {box === null ? (
          <div className="hs-box">
            <Field value={draft} onChange={(v) => { setDraft(v); setSent(false) }} placeholder={`Message ${peek.to}`} lines={[1, 5]} />
            <Button kind="secondary" disabled={!draft.trim()} onClick={doSend}>Send</Button>
          </div>
        ) : null}
        {sent ? <Text size="footnote" tone="secondary">Sent to its thread.</Text> : null}
        <div className="hs-actions">
          <Button kind="secondary" disabled={join !== null} title={join ?? undefined} onClick={doJoin}>Join</Button>
          <Button kind="secondary" disabled={stop !== null || stopped} title={stop ?? undefined} onClick={() => setConfirming(true)}>{stopped ? 'Stopping' : 'Stop'}</Button>
        </div>
        {join && stop ? <Text size="footnote" tone="secondary">{join}</Text> : null}
      </div>
      <Dialog
        open={confirming}
        title="Stop this handoff?"
        message={`${peek.to} stops what it is doing. You can pick the conversation up again from its thread.`}
        actions={[{ title: 'Stop', destructive: true, action: doStop }]}
        onClose={() => setConfirming(false)}
      />
    </SheetPage>
  )
}
