import { Button } from '../components/Controls'
import { Icon } from '../components/Icon'
import { HStack, VStack } from '../components/Stack'
import { Text } from '../components/Text'
import { useApp } from '../store/context'

/** The kith command line this computer's host runs agents on is missing, too old or failed to install. */
export function CliNoticeCard() {
  const notice = useApp().host.cliNotice
  if (!notice) return null
  return (
    <HStack align="flex-start" gap={12} className="update-needed">
      <Icon name="exclamationmark.triangle" size={15} color="var(--warning)" />
      <VStack gap={6} grow>
        <Text as="div" size="subheadline" weight="semibold" tone="text">The kith command line isn't ready</Text>
        <Text as="div" size="footnote" tone="secondary">{notice}</Text>
        <div className="update-needed-action">
          <Button onClick={() => window.kith.host.installCli()}>Install kith CLI</Button>
        </div>
      </VStack>
    </HStack>
  )
}
