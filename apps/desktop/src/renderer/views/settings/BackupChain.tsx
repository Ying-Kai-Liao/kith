import { useEffect } from 'react'
import {
  CLAUDE_ACCOUNTS_NOTE, addable, entryFor, hasClaudeAccount, loginCommand, mainChoices, mainEntry, mainSourceText, move,
  reasonText, stateText, type BackupEntry,
} from '@shared/backups'
import { Button, DropdownMenu, IconButton } from '../../components/Controls'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import { useApp } from '../../store/context'
import { BackupsStore } from '../../store/backups-store'
import type { BotStore } from '../../store/bot-store'
import { ComputerBadge } from '../ComputerBadge'
import './backup-chain.css'

// One store per computer, so Settings and a bot's details share one answer.
const stores = new WeakMap<BotStore, BackupsStore>()

/** The computer's backup chain, read again each time a view that shows it opens; null before `host` exists. */
export function useBackups(host: BotStore | null | undefined): BackupsStore | null {
  let model: BackupsStore | null = null
  if (host) {
    model = stores.get(host) ?? null
    if (!model) stores.set(host, (model = new BackupsStore(() => host.ready())))
  }
  useModel(model)
  useEffect(() => void model?.load(), [model])
  return model
}

/** Settings → Usage: every computer's backup order, under its limits. */
export function BackupChains({ only }: { only?: string } = {}) {
  const app = useApp()
  const hosts = app.computers.filter((c) => !only || c.id === only).flatMap((c) => app.store(c.id) ?? [])
  return (
    <>
      {hosts.map((host) => <ComputerBackups key={host.computer.id} host={host} labeled={hosts.length > 1} />)}
    </>
  )
}

function ComputerBackups({ host, labeled }: { host: BotStore; labeled: boolean }) {
  const store = useBackups(host)
  // A host from before this call has nothing to show.
  if (!store || store.unsupported) return null
  return (
    <section className="backups">
      <HStack gap={8} className="backups-head">
        {labeled ? <ComputerBadge computer={host.computer} size={16} /> : null}
        <Text as="h3" size="body" weight="semibold" tone="text">Backups</Text>
      </HStack>
      <Text size="callout" tone="secondary">
        Turns start on the main account. When it runs out of usage, Kith moves them down this list until the limit lifts.
        Logging in stays in the terminal; the app only shows the command.
      </Text>
      <MainAccountRow store={store} agent={null} />
      <ChainEditor store={store} agent={null} chain={store.backups.global} main={store.backups.main.global} />
    </section>
  )
}

/**
 * The account turns start on, above the chain: `agent` null is the computer's choice, which every agent of that
 * harness without one of its own takes. A change applies from the next turn; a session made on another account
 * isn't resumed, the agent starts fresh with the latest messages.
 */
export function MainAccountRow({ store, agent }: { store: BackupsStore; agent: string | null }) {
  const { backups } = store
  const main = agent ? backups.main.agents[agent] : null
  // A host or kith from before main accounts has nothing to show for an agent.
  if (agent && !main) return null
  const id = agent ? main!.id : backups.main.global
  const entry = main ? mainEntry(backups, main) : id ? entryFor(backups, id) : null
  const state = entry ? stateText(entry) : null
  const command = entry && state?.tone === 'blocked' ? loginCommand(entry) : null
  const choices = mainChoices(backups, main?.harness ?? null).filter((c) => c !== id)
  const reset = agent ? "Use the computer's main account" : "Each harness's default login"
  const own = agent ? main!.source === 'agent' : id !== null
  return (
    <VStack gap={4} className="backups-main">
      <HStack gap={8}>
        <Text size="callout" tone="secondary">Main account</Text>
        <Text truncate className="backups-id">{id ?? 'default login'}</Text>
        {state ? <Text size="callout" className={`backups-state ${state.tone}`}>{state.text}</Text> : null}
        <span className="backups-main-spacer" />
        <DropdownMenu
          className="styled-button secondary"
          title="Choose the main account"
          items={() => [
            ...(own ? [{ title: reset, action: () => void store.setMain(agent, null) }] : []),
            ...choices.map((c) => ({
              title: entryFor(backups, c).ready ? c : `${c} (${entryFor(backups, c).why ?? 'not ready'})`,
              action: () => void store.setMain(agent, c),
            })),
          ]}
        >
          Change
        </DropdownMenu>
      </HStack>
      <Text size="callout" tone="secondary">
        {main ? `From ${mainSourceText(main)}.` : id ? 'Every agent of its harness without a main account of its own starts here.' : "Each agent starts on its harness's own login."}
        {' '}A change applies from the next turn; a conversation from another account starts fresh with the latest messages.
      </Text>
      {command ? (
        <HStack gap={6} className="backups-fix">
          <Text size="callout" tone="secondary">Run in the terminal:</Text>
          <code className="backups-code">{command}</code>
          <IconButton title="Copy command" icon="doc.on.doc" size={22} onClick={() => void navigator.clipboard?.writeText(command)} />
        </HStack>
      ) : null}
    </VStack>
  )
}

/**
 * One order: its entries with their state, reorder, remove, and add. `agent` null is the global order. An entry
 * equal to `main` (the main account) is skipped, since turns already start there.
 */
export function ChainEditor({ store, agent, chain, main }: { store: BackupsStore; agent: string | null; chain: string[]; main?: string | null }) {
  const { backups } = store
  const save = (next: string[]) => void store.set(agent, next)
  const options = addable(backups, chain)
  return (
    <VStack gap={10}>
      {store.error ? <Text size="callout" tone="danger" role="alert">{store.error}</Text> : null}
      {!store.loaded && store.loading ? <Text size="callout" tone="secondary">Reading the backups…</Text> : null}
      {chain.length === 0 && store.loaded ? (
        <Text size="callout" tone="secondary">
          No backups. Add one below{agent ? '' : ', or from the terminal with `kith backup add codex`'}; nothing changes until you do.
        </Text>
      ) : null}
      {chain.length ? (
        <ol className="backups-list">
          {chain.map((id, i) => (
            <li key={id} className="backups-row">
              <EntryRow entry={entryFor(backups, id)} index={i} skipped={id === main} />
              <HStack gap={2} className="backups-actions">
                <IconButton title={`Move ${id} up`} icon="arrow.up" size={24} disabled={i === 0} onClick={() => save(move(chain, i, i - 1))} />
                <IconButton title={`Move ${id} down`} icon="arrow.down" size={24} disabled={i === chain.length - 1} onClick={() => save(move(chain, i, i + 1))} />
                <IconButton title={`Remove ${id}`} icon="xmark" size={24} onClick={() => save(chain.filter((c) => c !== id))} />
              </HStack>
            </li>
          ))}
        </ol>
      ) : null}
      <HStack gap={8}>
        <DropdownMenu
          className="styled-button secondary"
          title="Add a backup"
          items={() => options.map((id) => ({
            title: entryFor(backups, id).ready ? id : `${id} (${entryFor(backups, id).why ?? 'not ready'})`,
            action: () => save([...chain, id]),
          }))}
        >
          Add backup
        </DropdownMenu>
        {agent === null && store.loaded ? (
          <Button kind="secondary" disabled={store.loading} onClick={() => void store.load()}>Refresh</Button>
        ) : null}
      </HStack>
      {hasClaudeAccount(chain) ? <Text size="callout" tone="secondary" className="backups-note">{CLAUDE_ACCOUNTS_NOTE}</Text> : null}
    </VStack>
  )
}

function EntryRow({ entry, index, skipped }: { entry: BackupEntry; index: number; skipped?: boolean }) {
  const state = skipped ? { text: 'skipped: the main account', tone: 'blocked' as const } : stateText(entry)
  const command = state.tone === 'ok' ? null : loginCommand(entry)
  const why = state.tone === 'out' ? reasonText(entry.reason) : null
  return (
    <VStack gap={2} grow className="backups-entry">
      <HStack gap={8}>
        <Text tabular tone="tertiary" size="callout">{index + 1}</Text>
        <Text truncate className="backups-id">{entry.id}</Text>
        <Text size="callout" className={`backups-state ${state.tone}`}>{state.text}</Text>
      </HStack>
      {why ? <Text size="callout" tone="secondary">{why}</Text> : null}
      {command && state.tone === 'blocked' && !skipped ? (
        <HStack gap={6} className="backups-fix">
          <Text size="callout" tone="secondary">Run in the terminal:</Text>
          <code className="backups-code">{command}</code>
          <IconButton title="Copy command" icon="doc.on.doc" size={22} onClick={() => void navigator.clipboard?.writeText(command)} />
        </HStack>
      ) : null}
    </VStack>
  )
}
