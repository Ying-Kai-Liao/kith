import { useEffect } from 'react'
import {
  accountFor, addableTo, agentNames, move, pinText, setIds, statusText, windowText, withId, withoutId,
  type Account, type AgentAccounts, type Policy,
} from '@shared/accounts'
import { Button, CardSection, DropdownMenu, IconButton, SegmentedChoice, Switch, ValueRow } from '../../components/Controls'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import { useApp } from '../../store/context'
import { AccountsStore } from '../../store/accounts-store'
import type { BotStore } from '../../store/bot-store'
import { AccountLogins } from './AccountLogins'
import './accounts.css'

// One store per computer, so reopening Settings shows the last answer while it reads again.
const stores = new WeakMap<BotStore, AccountsStore>()

function useAccounts(host: BotStore | null | undefined): AccountsStore | null {
  let model: AccountsStore | null = null
  if (host) {
    model = stores.get(host) ?? null
    if (!model) stores.set(host, (model = new AccountsStore(() => host.ready())))
  }
  useModel(model)
  useEffect(() => void model?.load(), [model])
  return model
}

/**
 * Settings → Accounts (owner only): the computer's managed account set. The kernel owns the set, its order, the policy,
 * the pins and which accounts are out; this page shows that and sends whole lists back. The older Backups section
 * under Usage reads and writes the same kernel state, so both always agree.
 */
export function AccountsView() {
  const app = useApp()
  const host = app.local
  const store = useAccounts(host)
  if (!host || !store) return <CardSection><Text tone="secondary">Not connected to a computer.</Text></CardSection>
  if (store.unsupported) {
    return (
      <CardSection footer="This computer's Kith is older than the account set. Update Kith on this computer, then open this page again.">
        <Text tone="secondary">Update Kith on this computer to manage accounts here.</Text>
      </CardSection>
    )
  }
  const { accounts } = store
  const ids = setIds(accounts)
  const busy = store.saving
  return (
    <>
      {store.error ? <CardSection><Text tone="danger" role="alert">{store.error}</Text></CardSection> : null}
      {!store.loaded && store.loading ? <CardSection><Text tone="secondary">Reading the accounts…</Text></CardSection> : null}

      <CardSection
        title="Account set"
        footer="Turns use the accounts in this order, each until it is out of usage (fill), or share the work across them (spread). Logging in happens in a terminal on the computer; the app never sees a token."
      >
        <ValueRow label="Use accounts">
          <SegmentedChoice<Policy>
            selection={accounts.policy}
            options={[{ id: 'fill', label: 'In order' }, { id: 'spread', label: 'Spread' }]}
            onChange={(p) => { if (!busy && p !== accounts.policy) void store.setPolicy(p) }}
          />
        </ValueRow>
        <ValueRow label="Across Claude and Codex" detail="An agent may move to Codex when its Claude accounts are out. Only for agents with Codex in their set.">
          <Switch on={accounts.crossHarness} disabled={busy} label="Cross-harness" onChange={(on) => void store.setCrossHarness(null, on ? 'on' : 'off')} />
        </ValueRow>
      </CardSection>

      <CardSection title="Order">
        <AccountList store={store} agent={null} ids={ids} empty="No accounts in the set. Add one below." />
      </CardSection>

      <CardSection title="Logins">
        <AccountLogins store={store} host={host} />
      </CardSection>

      <CardSection title="Not in the set">
        {accounts.accounts.filter((a) => !a.inSet).length === 0 ? <Text tone="secondary">Every account is in the set.</Text> : null}
        {accounts.accounts.filter((a) => !a.inSet).map((a) => (
          <AccountRow key={a.id} store={store} account={a} actions={
            <Button kind="secondary" disabled={busy} onClick={() => void store.setList(null, withId(ids, a.id))}>Add to set</Button>
          } />
        ))}
      </CardSection>

      <AgentCards store={store} />
    </>
  )
}

/** One account's line: id, kind, state, fix hint, usage windows, pins. `actions` sit at the right. */
function AccountRow({ store, account, actions }: { store: AccountsStore; account: Account; actions?: React.ReactNode }) {
  const state = statusText(account)
  const busy = store.saving
  return (
    <VStack gap={4} className="accounts-row">
      <HStack gap={8}>
        <Text truncate className="accounts-id">{account.id}</Text>
        <Text size="caption" tone="secondary">{account.kind}</Text>
        <span className="accounts-spacer" />
        {account.status === 'out' ? <Button kind="secondary" disabled={busy} onClick={() => void store.clear(account.id)}>It's back</Button> : null}
        {actions}
      </HStack>
      <Text size="callout" className={`accounts-state ${state.tone}`}>{state.text}</Text>
      {account.status === 'signed_out' && account.fix ? (
        <HStack gap={6} className="accounts-fix">
          <Text size="callout" tone="secondary">Run in the terminal:</Text>
          <code className="accounts-code">{account.fix}</code>
          <IconButton title="Copy command" icon="doc.on.doc" size={22} onClick={() => void navigator.clipboard?.writeText(account.fix!)} />
        </HStack>
      ) : null}
      {account.usage && account.usage.windows.length > 0 ? (
        <Text size="caption" tone="secondary">{account.usage.windows.map((w) => windowText(w)).join(' · ')}</Text>
      ) : null}
      {account.pins.length > 0 ? <Text size="caption" tone="secondary">Pinned: {account.pins.map(pinText).join(', ')}</Text> : null}
    </VStack>
  )
}

/** An ordered list: reorder, take out, add. `agent` null is the global set; otherwise that agent's own list. */
function AccountList({ store, agent, ids, empty }: { store: AccountsStore; agent: string | null; ids: string[]; empty: string }) {
  const { accounts } = store
  const busy = store.saving
  const save = (next: string[]) => void store.setList(agent, next)
  const options = addableTo(accounts, ids)
  return (
    <VStack gap={8}>
      {ids.length === 0 ? <Text tone="secondary">{empty}</Text> : null}
      {ids.map((id, i) => {
        const account = accountFor(accounts, id)
        const controls = (
          <HStack gap={2}>
            <IconButton title={`Move ${id} up`} icon="arrow.up" size={24} disabled={busy || i === 0} onClick={() => save(move(ids, i, i - 1))} />
            <IconButton title={`Move ${id} down`} icon="arrow.down" size={24} disabled={busy || i === ids.length - 1} onClick={() => save(move(ids, i, i + 1))} />
            <IconButton title={`Take ${id} out`} icon="xmark" size={24} disabled={busy} onClick={() => save(withoutId(ids, id))} />
          </HStack>
        )
        return account
          ? <AccountRow key={id} store={store} account={account} actions={controls} />
          : <HStack key={id} gap={8}><Text truncate className="accounts-id">{id}</Text><Text size="callout" tone="secondary">not known to this computer</Text><span className="accounts-spacer" />{controls}</HStack>
      })}
      {options.length > 0 ? (
        <HStack gap={8}>
          <DropdownMenu
            className="styled-button secondary"
            title="Add to the list"
            items={() => options.map((a) => ({ title: a.ready ? a.id : `${a.id} (${statusText(a).text})`, action: () => save(withId(ids, a.id)) }))}
          >
            Add to the list
          </DropdownMenu>
        </HStack>
      ) : null}
    </VStack>
  )
}

function AgentCards({ store }: { store: AccountsStore }) {
  const agents = agentNames(store.accounts)
  if (agents.length === 0) return null
  return (
    <>
      {agents.map(({ name, view }) => (
        <CardSection key={name} title={name}>
          {view ? <AgentCard store={store} name={name} view={view} /> : (
            <Text tone="secondary" role="status">Unavailable. Kith could not read this agent's accounts just now; its settings are unchanged.</Text>
          )}
        </CardSection>
      ))}
    </>
  )
}

function AgentCard({ store, name, view }: { store: AccountsStore; name: string; view: AgentAccounts }) {
  const { accounts } = store
  const busy = store.saving
  const global = setIds(accounts)
  const pinChoices = accounts.accounts.filter((a) => a.id !== view.pin?.id)
  return (
    <VStack gap={10} className="accounts-agent">
      <Text size="callout" tone="secondary">
        Order: {view.order.length ? view.order.join(' → ') : 'none'}
        {view.candidates.length ? `. Next to try: ${view.candidates.join(', ')}` : ''}
      </Text>
      {view.allOut ? <Text size="callout" className="accounts-state out" role="status">Every account for this agent is out; turns wait until one is back.</Text> : null}
      {view.skipped.map((s) => (
        <Text key={s.id} size="callout" tone="secondary">Skipped {s.id}: {statusText(s).text}</Text>
      ))}

      <HStack gap={8} className="accounts-pin">
        <Text size="callout" tone="secondary">Pin</Text>
        <Text truncate className="accounts-id">{view.pin ? view.pin.id : 'none'}</Text>
        <span className="accounts-spacer" />
        {view.pin ? (
          <>
            <Switch on={view.pin.only} disabled={busy} label={`${name} uses only ${view.pin.id}`} onChange={(only) => void store.pin(name, view.pin!.id, only)}>Only</Switch>
            <Button kind="secondary" disabled={busy} onClick={() => void store.unpin(name)}>Unpin</Button>
          </>
        ) : null}
        <DropdownMenu
          className="styled-button secondary"
          title={`Pin an account for ${name}`}
          items={() => pinChoices.map((a) => ({ title: a.id, action: () => void store.pin(name, a.id, view.pin?.only ?? false) }))}
        >
          {view.pin ? 'Change' : 'Pin'}
        </DropdownMenu>
      </HStack>

      <VStack gap={6}>
        <HStack gap={8}>
          <Text size="callout" tone="secondary" grow>{view.own ? 'Its own list' : 'Follows the global set'}</Text>
          {view.own
            ? <Button kind="secondary" disabled={busy} onClick={() => void store.setList(name, null)}>Use the global set</Button>
            : <Button kind="secondary" disabled={busy} onClick={() => void store.setList(name, global)}>Give it its own list</Button>}
        </HStack>
        {view.own ? <AccountList store={store} agent={name} ids={view.own} empty="Its list is empty." /> : null}
      </VStack>

      <HStack gap={8}>
        <Text size="callout" tone="secondary" grow>
          Across Claude and Codex: {view.crossHarness ? 'on' : 'off'}. Only applies when Codex is in this agent's set.
        </Text>
        <DropdownMenu
          className="styled-button secondary"
          title={`Cross-harness for ${name}`}
          items={() => [
            { title: 'On', action: () => void store.setCrossHarness(name, 'on') },
            { title: 'Off', action: () => void store.setCrossHarness(name, 'off') },
            { title: 'Follow the computer', action: () => void store.setCrossHarness(name, 'reset') },
          ]}
        >
          Change
        </DropdownMenu>
      </HStack>
    </VStack>
  )
}
