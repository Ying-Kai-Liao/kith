import { SegmentedChoice } from '../../../components/Controls'
import { Text } from '../../../components/Text'
import { useStoreRef } from '../../../store/context'
import { ChainEditor, MainAccountRow, useBackups } from '../../settings/BackupChain'
import '../../settings/backup-chain.css'

/** A Kith bot's main account, and its own backup order or the global one: where its turns go when that account runs out of usage. */
export function AgentBackups({ agent }: { agent: string }) {
  const store = useBackups(useStoreRef())
  if (!store || store.unsupported) return null
  const own = store.backups.agents[agent]
  const choice = own ? 'custom' : 'global'
  const main = store.backups.main.agents[agent]?.id ?? null
  return (
    <section className="details-section">
      <span className="details-section-title">Backups</span>
      <div className="outlined-card kith-card backups-choice">
        <MainAccountRow store={store} agent={agent} />
        <SegmentedChoice
          selection={choice}
          options={[{ id: 'global', label: 'Use the global order' }, { id: 'custom', label: 'Custom order' }]}
          // A custom order starts as a copy of the global one, so choosing it changes nothing until it is edited.
          onChange={(id) => void store.set(agent, id === 'global' ? null : [...store.backups.global])}
        />
        {own ? (
          <ChainEditor store={store} agent={agent} chain={own} main={main} />
        ) : (
          <Text size="callout" tone="secondary">
            {store.backups.global.length ? `Follows the global order: ${store.backups.global.map((id) => (id === main ? `${id} (skipped: the main account)` : id)).join(', ')}.` : 'No backups are set. Add them in Settings, or give this bot a custom order.'}
          </Text>
        )}
      </div>
    </section>
  )
}
