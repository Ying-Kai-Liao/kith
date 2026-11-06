import { useEffect, useState } from 'react'
import { KERNEL_FALLBACK_HINT, KERNEL_FALLBACK_PROBLEM } from '@shared/new-agent'
import { Button } from '../components/Controls'
import { Icon } from '../components/Icon'
import { HStack, VStack } from '../components/Stack'
import { Text } from '../components/Text'
import type { BotStore } from '../store/bot-store'

/**
 * Says so when Kith's kernel can't run (Python or uv missing or broken), with what to do. Every agent depends on it, so
 * the app asks the host when it connects and shows the answer here; "Check again" asks the host afresh. Nothing when it is fine
 * or the host can't say.
 */
export function KernelNotice({ store, compact = false }: { store: BotStore; compact?: boolean }) {
  const online = store.connection.kind === 'online'
  const [checking, setChecking] = useState(false)
  useEffect(() => {
    if (online) void store.checkKernel()
  }, [store, online])
  const status = store.kernel
  if (!online || !status || status.ok) return null
  const again = async () => {
    setChecking(true)
    await store.checkKernel(true)
    setChecking(false)
  }
  return (
    <HStack align="flex-start" gap={12} className="update-needed" role="alert">
      <Icon name="exclamationmark.triangle" size={15} color="var(--warning)" />
      <VStack gap={6} grow>
        <Text as="div" size="subheadline" weight="semibold" tone="text">Kith can't run agents on {store.hostName}</Text>
        <Text as="div" size="footnote" tone="secondary">{status.problem ?? KERNEL_FALLBACK_PROBLEM}</Text>
        <Text as="div" size="footnote" tone="secondary">{status.hint ?? KERNEL_FALLBACK_HINT}</Text>
        {compact ? null : (
          <div className="update-needed-action">
            <Button kind="secondary" disabled={checking} onClick={() => { void again() }}>{checking ? 'Checking…' : 'Check again'}</Button>
          </div>
        )}
      </VStack>
    </HStack>
  )
}
