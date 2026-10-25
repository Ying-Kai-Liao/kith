import { useCallback, useEffect, useState } from 'react'
import { ago } from '@shared/bot-guests'
import { keysByService, type KeyRow } from '@shared/secret-card'
import { Button, CardSection, Spinner, ValueRow } from '../../components/Controls'
import { Dialog } from '../../components/Overlay'
import { VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useApp } from '../../store/context'
import { errorText } from './parts'

/** One agent's hold on one key, asked to be removed. */
interface Removal {
  key: KeyRow
  agent: string
}

/**
 * The service keys saved on this Mac, by service: the account, the agents that may use it, when it was last used.
 * Owner only (the page is hidden for members). It never shows or fetches a value; adding a key happens in a chat card.
 */
export function KeysView() {
  const app = useApp()
  const store = app.local
  const [keys, setKeys] = useState<KeyRow[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [removal, setRemoval] = useState<Removal | null>(null)
  const [working, setWorking] = useState<string | null>(null)
  const online = store?.connection.kind === 'online'

  const refresh = useCallback(async () => {
    if (!store) return
    try {
      setKeys(await store.listKeys())
      setError(null)
    } catch (e) {
      setError(errorText(e))
    }
  }, [store])
  useEffect(() => void (online && refresh()), [online, refresh])

  const confirm = async () => {
    if (!store || !removal) return
    const { key, agent } = removal
    setRemoval(null)
    setWorking(`${key.profile}/${agent}`)
    try {
      await store.revokeKey(agent, key.service, key.profile)
      await refresh()
    } catch (e) {
      setError(errorText(e))
    }
    setWorking(null)
  }

  if (keys === null) {
    return <CardSection>{error ? <Text tone="secondary">{error}</Text> : <Spinner size={14} />}</CardSection>
  }
  if (keys.length === 0) {
    return <CardSection footer="When an agent needs a service's key, a card in your chat with it asks for the key."><Text tone="secondary">No keys saved yet.</Text></CardSection>
  }
  const now = Date.now()
  return (
    <>
      {error ? <CardSection><Text tone="secondary">{error}</Text></CardSection> : null}
      {keysByService(keys).map((group) => (
        <CardSection key={group.service} title={group.service} footer="Keys live in the Keychain on this Mac. Removing access stops that agent from using the key.">
          {group.keys.map((key) => (
            <VStack key={key.profile} gap={2} className="settings-key">
              <ValueRow label={key.account} detail={key.lastUsed ? `Last used ${ago(Date.parse(key.lastUsed), now)}` : 'Never used'} />
              {key.agents.length === 0 ? <Text tone="secondary">No agent has this key.</Text> : null}
              {key.agents.map((agent) => (
                <ValueRow key={agent} label={agent}>
                  {working === `${key.profile}/${agent}` ? <Spinner size={12} /> : null}
                  <Button kind="secondary" disabled={working !== null} onClick={() => setRemoval({ key, agent })}>Remove access</Button>
                </ValueRow>
              ))}
            </VStack>
          ))}
        </CardSection>
      ))}
      <Dialog
        open={removal !== null}
        title={removal ? `Remove ${removal.agent}'s access to ${removal.key.profile}?` : ''}
        message="The agent can no longer use this key. The key stays saved; you can give it access again later."
        actions={[{ title: 'Remove access', destructive: true, action: () => void confirm() }]}
        onClose={() => setRemoval(null)}
      />
    </>
  )
}
