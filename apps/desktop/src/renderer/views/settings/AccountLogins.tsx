import { useState } from 'react'
import { removable, stateText, type BackupEntry } from '@shared/backups'
import { Button, IconButton, SegmentedChoice } from '../../components/Controls'
import { Dialog } from '../../components/Overlay'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { StoreContext } from '../../store/context'
import type { BackupsStore } from '../../store/backups-store'
import type { BotStore } from '../../store/bot-store'
import { SetupTerminalView } from '../marketplace/SetupTerminal'
import { FormSheet } from '../marketplace/pieces'

type Harness = 'claude' | 'codex'
/** An account name as kith takes it; kith checks it again. */
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/

/**
 * The extra logins on one computer (`claude:work`, `codex:deep`): add one (kith makes the folder, the computer runs
 * its login in a terminal shown here) or remove one. Both ask the host, which asks the kith command.
 */
export function AccountLogins({ store, host }: { store: BackupsStore; host: BotStore }) {
  const [adding, setAdding] = useState<{ harness: Harness; name: string } | null>(null)
  const [removing, setRemoving] = useState<BackupEntry | null>(null)
  const [note, setNote] = useState<string | null>(null)
  const accounts = removable(store.backups)

  const remove = async (entry: BackupEntry) => {
    const [harness, name] = entry.id.split(':')
    if (!harness || !name) return
    setNote(null)
    const inChain = await store.removeAccount(harness, name)
    if (inChain) setNote(`${entry.id} was in a backup order; take it out of the order too.`)
  }

  return (
    <VStack gap={8} className="backups-logins">
      <HStack gap={8}>
        <Text size="callout" tone="secondary" grow>Logins you added</Text>
        <Button kind="secondary" onClick={() => setAdding({ harness: 'claude', name: '' })}>Add login</Button>
      </HStack>
      {accounts.length === 0 && store.loaded ? (
        <Text size="callout" tone="secondary">None. Add a Claude or Codex login to use in the orders above.</Text>
      ) : null}
      {accounts.map((entry) => {
        const state = stateText(entry)
        return (
          <HStack key={entry.id} gap={8} className="backups-login">
            <Text truncate className="backups-id">{entry.id}</Text>
            <Text size="callout" className={`backups-state ${state.tone}`}>{state.text}</Text>
            <span className="backups-main-spacer" />
            {/* Adding it again makes no second folder: kith hands back the same login to run. */}
            {!entry.ready && entry.account && (entry.harness === 'claude' || entry.harness === 'codex')
              ? <Button kind="secondary" onClick={() => setAdding({ harness: entry.harness as Harness, name: entry.account! })}>Log in</Button>
              : null}
            <IconButton title={`Remove ${entry.id}`} icon="trash" size={24} onClick={() => setRemoving(entry)} />
          </HStack>
        )
      })}
      {note ? <Text size="callout" tone="secondary" role="status">{note}</Text> : null}
      <Dialog
        open={removing !== null}
        title={`Remove ${removing?.id ?? 'login'}?`}
        message="This deletes the login kept for it on this computer. It can't be undone; add it again to log in again."
        actions={[{ title: 'Remove', destructive: true, action: () => removing && void remove(removing) }]}
        onClose={() => setRemoving(null)}
      />
      <FormSheet open={adding !== null} onClose={() => { setAdding(null); void store.load() }} width={560} height={560}>
        {adding ? (
          <StoreContext.Provider value={host}>
            <AddLogin store={store} initial={adding} done={() => { setAdding(null); void store.load() }} />
          </StoreContext.Provider>
        ) : null}
      </FormSheet>
    </VStack>
  )
}

/** Name it, confirm the Claude warning, then watch the login in a terminal on the computer. */
function AddLogin({ store, initial, done }: { store: BackupsStore; initial: { harness: Harness; name: string }; done: () => void }) {
  const [harness, setHarness] = useState<Harness>(initial.harness)
  const [name, setName] = useState(initial.name)
  const [warning, setWarning] = useState<string | null>(null)
  const [term, setTerm] = useState<{ id: string; label: string } | null>(null)
  const [busy, setBusy] = useState(false)

  const add = async (yes: boolean) => {
    setBusy(true)
    try {
      const got = await store.addAccount(harness, name, yes, 80, 24)
      if (got.kind === 'warning') setWarning(got.warning)
      else setTerm({ id: got.term, label: got.id })
    } catch {
      // The store shows the host's words.
    } finally {
      setBusy(false)
    }
  }

  if (term) {
    return (
      <SetupTerminalView
        backend={null}
        step="login"
        title={`Log in ${term.label}`}
        open={async () => term.id}
        finished={() => store.load()}
        back={done}
      />
    )
  }
  const valid = NAME.test(name)
  return (
    <VStack gap={12} className="backups-add">
      <Text as="h2" size="headline">Add a login</Text>
      <SegmentedChoice
        selection={harness}
        options={[{ id: 'claude', label: 'Claude' }, { id: 'codex', label: 'Codex' }]}
        onChange={setHarness}
      />
      <input
        className="settings-input"
        placeholder="Name, such as work"
        maxLength={40}
        value={name}
        spellCheck={false}
        aria-label="Login name"
        autoFocus
        onChange={(e) => setName(e.target.value.trim())}
      />
      <Text size="callout" tone="secondary">
        Kith makes a private folder for it, then runs the {harness === 'claude' ? 'Claude' : 'Codex'} login here. Kith never sees the token.
      </Text>
      {store.error ? <Text size="callout" tone="danger" role="alert">{store.error}</Text> : null}
      <HStack gap={8}>
        <Button disabled={!valid || busy} onClick={() => void add(false)}>Add login</Button>
        <Button kind="secondary" onClick={done}>Cancel</Button>
      </HStack>
      <Dialog
        open={warning !== null}
        title="Add another Claude account?"
        message={warning}
        actions={[{ title: 'Add account', action: () => void add(true) }]}
        onClose={() => setWarning(null)}
      />
    </VStack>
  )
}
