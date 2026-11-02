import { useEffect, useState } from 'react'
import type { Bot } from '@shared/models'
import { EFFORT_LEVELS, HARNESSES } from '@shared/bot-sections'
import { engineNotRunHere, madeButFailed, newAgentDescriptionError, newAgentNameError, newAgentParams } from '@shared/new-agent'
import { CardForm, CardSection, ChoicePicker, IconButton, Spinner, ValueRow } from '../../components/Controls'
import { ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { useStore } from '../../store/context'
import { KernelNotice } from '../KernelNotice'
import { Field } from '../marketplace/pieces'
import { errorText } from './drafts'

const ENGINES = [{ id: '', label: 'Default' }, ...HARNESSES]

/** A name typed in a search box, made into the closest valid agent name to start from. */
export const suggestedName = (typed: string) => typed.trim().toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^[^a-z]+|-+$/g, '').slice(0, 31)

/**
 * "New agent": every bot is a kith agent, so it needs a name and a line about what it does before it is made.
 * `onMade` gets the new bot to open; the sheet stays open with the host's own words when kith refuses (a name taken).
 */
export function NewAgentSheet({ open, initialName, onClose, onMade }: {
  open: boolean
  initialName: string
  onClose: () => void
  onMade: (bot: Bot) => void
}) {
  return (
    <Sheet open={open} onClose={onClose} width={480}>
      {open ? <Page initialName={initialName} onClose={onClose} onMade={onMade} /> : null}
    </Sheet>
  )
}

function Page({ initialName, onClose, onMade }: { initialName: string; onClose: () => void; onMade: (bot: Bot) => void }) {
  const store = useStore()
  const [name, setName] = useState(() => suggestedName(initialName))
  const [description, setDescription] = useState('')
  const [harness, setHarness] = useState('')
  const [model, setModel] = useState('')
  const [effort, setEffort] = useState('default')
  const [running, setRunning] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // Made, but this app can't run its engine: nothing to open.
  const [madeOnly, setMadeOnly] = useState<string | null>(null)

  // Looking again each time the form opens: the person may have just fixed it.
  useEffect(() => {
    void store.checkKernel(true)
  }, [store])

  const kernelDown = store.kernel !== null && !store.kernel.ok
  const nameProblem = name ? newAgentNameError(name) : null
  const descriptionProblem = description ? newAgentDescriptionError(description) : null
  const ready = !newAgentNameError(name) && !newAgentDescriptionError(description) && !kernelDown
  const create = async () => {
    if (running || !ready) return
    setRunning(true)
    setError(null)
    try {
      const { agent, bot } = await store.newKithAgent(newAgentParams({ name, description, harness, model, effort }))
      if (bot) return onMade(bot)
      setMadeOnly(engineNotRunHere(agent))
    } catch (e) {
      const message = errorText(e)
      if (madeButFailed(message, name.trim())) {
        // The agent exists: open it, and keep the host's words where they can be seen.
        const bot = await store.waitForAgentBot(name.trim()).catch(() => null)
        store.setError(message)
        if (bot) return onMade(bot)
      }
      setError(message)
    }
    setRunning(false)
  }

  return (
    <SheetPage>
      <ModalHeader
        title="New agent"
        trailing={running ? <Spinner /> : madeOnly ? null : <IconButton title="Create" icon="checkmark" disabled={!ready} onClick={() => { void create() }} />}
      />
      <CardForm>
        {madeOnly ? (
          <CardSection footer={madeOnly}>
            <ValueRow label="Done">
              <IconButton title="Close" icon="xmark" onClick={onClose} />
            </ValueRow>
          </CardSection>
        ) : (
          <>
            <KernelNotice store={store} />
            <CardSection footer={error ?? nameProblem ?? 'Lowercase letters, numbers and dashes. It is how you open the agent from Terminal too.'}>
              <Field value={name} onChange={(v) => { setName(v); setError(null) }} label="Name" placeholder="trip-planner" />
            </CardSection>
            <CardSection footer={descriptionProblem ?? 'One line: what it does.'}>
              <Field value={description} onChange={setDescription} label="Description" placeholder="Plans trips and books the boring parts" />
            </CardSection>
            <CardSection title="Optional">
              <ValueRow label="Engine">
                <ChoicePicker selection={harness} options={ENGINES} onChange={setHarness} />
              </ValueRow>
              <ValueRow label="Model">
                <input className="field-box" value={model} placeholder="Agent's default" aria-label="Model" spellCheck={false} onChange={(e) => setModel(e.target.value)} />
              </ValueRow>
              <ValueRow label="Effort">
                <ChoicePicker selection={effort} options={EFFORT_LEVELS} onChange={setEffort} />
              </ValueRow>
            </CardSection>
          </>
        )}
      </CardForm>
    </SheetPage>
  )
}
