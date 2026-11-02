import { Button } from '../components/Controls'
import { Icon } from '../components/Icon'
import { Text } from '../components/Text'
import { useApp } from '../store/context'

const isMac = window.kith.platform === 'darwin'

/** What stands between this computer and the chat, with the one action that fixes it. */
export function HostStateView({ compact = false }: { compact?: boolean }) {
  const app = useApp()
  const state = app.host.state
  switch (state.kind) {
    case 'notInstalled':
      return <EmptyState compact={compact} icon="desktopcomputer" title={`Set up this ${isMac ? 'Mac' : 'computer'}`} message={`Kith runs your coding agents through the host, a small background service on this ${isMac ? 'Mac' : 'computer'}.`} action={['Install host', () => window.kith.host.install()]} />
    case 'missingBinary':
      return <EmptyState compact={compact} icon="desktopcomputer.trianglebadge.exclamationmark" title="The host is missing" message="This copy of Kith doesn't include its host. Download Kith again from github.com/Ying-Kai-Liao/kith." />
    case 'failed':
      return <EmptyState compact={compact} icon="desktopcomputer.trianglebadge.exclamationmark" title="The host isn't running" message={state.message} action={['Try again', () => window.kith.host.restart()]} />
    default:
      return <EmptyState compact={compact} icon="desktopcomputer" title="Starting the host…" message="This takes a few seconds." />
  }
}

/** A quiet placeholder for an empty screen: icon, title, a line of help and an optional action. */
export function EmptyState({ icon, title, message, action, compact = false }: { icon: string; title: string; message: string; action?: [string, () => void]; compact?: boolean }) {
  return (
    <div className={`empty-state ${compact ? 'local-recovery' : ''}`}>
      <Icon name={icon} size={compact ? 22 : 36} weight="light" color="var(--tertiary)" />
      <Text as="div" size={compact ? 'callout' : 'title3'} weight="semibold" tone="text">{title}</Text>
      <Text as="div" size="callout" tone="secondary" className="empty-state-message">{message}</Text>
      {action ? (
        <Button kind={compact ? 'secondary' : 'primary'} onClick={action[1]} className="empty-state-action">
          {action[0]}
        </Button>
      ) : null}
    </div>
  )
}

