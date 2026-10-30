import { useEffect, useState } from 'react'
import { shareMessage } from '@shared/share-link'
import { Button, ChoiceList } from '../components/Controls'
import { ModalHeader, Sheet, SheetPage } from '../components/Overlay'
import { HStack, VStack } from '../components/Stack'
import { Text } from '../components/Text'
import { useModel } from '../lib/observable'
import { useApp } from '../store/context'
import { parseRef, refKey } from '../store/app-model'
import { shareOffer } from '../store/share-offer'
import './share.css'

/** Something shared to Kith from another app (`kith://share?...`): add a note, pick a chat, send. */
export function ShareSheet() {
  const offer = useModel(shareOffer)
  const app = useApp()
  const [note, setNote] = useState('')
  const [chosen, setChosen] = useState<string | null>(null)
  // Something that arrived while the app was starting, and something that arrives while it runs.
  useEffect(() => void shareOffer.start(), [])
  useEffect(() => window.kith.app.onCommand((c) => c.kind === 'shareReceived' && void shareOffer.start()), [])
  const item = offer.item
  useEffect(() => {
    if (item) setNote('')
  }, [item])
  const roster = app.roster
  const several = new Set(roster.map((r) => r.computer.id)).size > 1
  // Starts on the chat that is open, else the first one.
  const selected = chosen && roster.some((r) => refKey(r.ref) === chosen) ? chosen : app.selection ? refKey(app.selection) : roster[0] ? refKey(roster[0].ref) : null

  const send = () => {
    const ref = selected ? parseRef(selected) : null
    if (!item || !ref) return
    app.store(ref.computerId)?.send(shareMessage(item, note), ref.botId)
    app.select(ref)
    shareOffer.dismiss()
  }

  return (
    <Sheet className="settings-sheet" open={item !== null} onClose={() => shareOffer.dismiss()} width={420}>
      <SheetPage>
        <ModalHeader title="Send to Kith" onClose={() => shareOffer.dismiss()} />
        <VStack gap={14} className="share-body">
          {item ? (
            <VStack gap={2}>
              {item.title ? <Text weight="semibold" className="share-clip">{item.title}</Text> : null}
              <Text size="callout" tone="secondary" className="share-clip">{item.link ?? item.text}</Text>
            </VStack>
          ) : null}
          <textarea className="share-note" value={note} placeholder="Add a note" rows={2} onChange={(e) => setNote(e.target.value)} />
          {roster.length === 0 ? (
            <Text size="callout" tone="secondary">There are no chats yet. Connect a computer first.</Text>
          ) : (
            <div className="share-chats">
              <ChoiceList
                selection={selected ?? ''}
                options={roster.map((r) => ({ id: refKey(r.ref), label: r.bot.name, detail: several ? r.computer.name : null }))}
                onChange={setChosen}
              />
            </div>
          )}
          <HStack gap={8} justify="end">
            <Button kind="secondary" onClick={() => shareOffer.dismiss()}>Cancel</Button>
            <Button onClick={send} disabled={!selected || roster.length === 0}>Send</Button>
          </HStack>
        </VStack>
      </SheetPage>
    </Sheet>
  )
}
