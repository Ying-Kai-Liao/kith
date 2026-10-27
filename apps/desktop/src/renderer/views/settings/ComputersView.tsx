import { useCallback, useEffect, useState } from 'react'
import type { AccountApproval, AuthorizedDevice, PairingInfo } from '@shared/models'
import { newSSHProfile, type SSHProfile } from '@shared/ssh'
import { Button, CardSection, IconButton, Spinner, Switch } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { AnchoredMenu, Dialog, ModalHeader, Sheet, useContextMenu, useDismiss, type MenuItem } from '../../components/Overlay'
import { HStack, IconSlot, Spacer, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import { prefs, usePref } from '../../lib/prefs'
import { useAccount } from '../../store/account'
import type { BotStore } from '../../store/bot-store'
import { StoreContext, useApp } from '../../store/context'
import { macRows, spaced, type MacRow } from '../../store/mac-list'
import { AccountShareSlot } from './AccountShareSlot'
import { MacProblemCards } from './MacProblemCard'
import { ComputerBadge } from '../ComputerBadge'
import { computerSelection } from '../ComputerFilterHeader'
import { deviceIcon, errorText, liveClient, relativeDay, Reveal, thisMac } from './parts'
import { useSSH } from './ssh-model'
import { SSHRow } from './SSHRow'
import { BackupChains } from './BackupChain'
import { VoiceChatSettingsView } from './VoiceChatSettingsView'
import { PairingCode, type PairingSource } from '../PairingCode'

/**
 * The Account pane's devices: every computer this account can use with its Show in Crew switch (this Mac and SSH
 * computers open their sheet), the buttons that add a computer, and SSH computers.
 */
export function AccountDevices({ owner }: { owner: boolean }) {
  const app = useApp()
  const account = useAccount()
  const sshState = useSSH()
  const macs = useModel(app.relayMacs)
  useEffect(() => void macs.refresh(), [macs, account.user?.userId])
  const [editing, setEditing] = useState<{ profile: SSHProfile; isNew: boolean } | null>(null)
  const [panel, setPanel] = useState<{ kind: 'voice' | 'backups'; store: BotStore } | null>(null)
  // An id, not a store, so a second click on a row can't stack a second sheet.
  const [device, setDevice] = useState<string | null>(null)
  const [pairing, setPairing] = useState<BotStore | null>(null)
  const [hidden, setHidden] = usePref(prefs.hiddenComputers)
  const rows = macRows(app, (id) => sshState.isSSH(id))
  const selection = computerSelection(app.computers.map((c) => c.id), hidden)
  const thisStore = app.managedStores.find((s) => !sshState.isSSH(s.computer.id)) ?? null
  const deviceStore = device ? app.managedStores.find((s) => s.computer.id === device) ?? null : null
  useModel(thisStore)

  return (
    <>
      {owner ? <MacProblemCards rows={rows} shown={selection.shown} /> : null}
      <AccountShareSlot />
      {owner ? (
        <>
          <CardSection title="Your devices" accessory={<span className="settings-section-note">Show in Crew</span>}>
            {rows.length === 0
              ? [<Text key="none" tone="secondary">{account.isSignedIn ? (macs.loaded ? 'No computers yet.' : 'Looking for your computers…') : 'Sign in to reach your other Macs.'}</Text>]
              : rows.map((row) => (
                  <ComputerRow
                    key={row.id}
                    row={row}
                    shown={selection.shown.includes(row.id)}
                    onShow={() => setHidden(selection.toggling(row.id))}
                    openPanel={(kind, store) => setPanel({ kind, store })}
                    onOpen={row.kind === 'relay' ? undefined : () => setDevice(row.id)}
                  />
                ))}
          </CardSection>
          {macs.error ? <Text as="div" size="caption" tone="attention">{macs.error}</Text> : null}

          <HStack gap={8} className="settings-account-buttons">
            <Button kind="secondary" disabled={!thisStore || thisStore.connection.kind !== 'online'} onClick={() => thisStore && setPairing(thisStore)}>Pair iPhone…</Button>
            <Button kind="secondary" onClick={() => setEditing({ profile: newSSHProfile(window.kith.hostPort), isNew: true })}>Add a Mac over SSH…</Button>
          </HStack>

          {sshState.state.profiles.length > 0 ? (
            <CardSection title="Over SSH" footer={`Run bots on another computer you reach with SSH. It needs kith-serve installed; your SSH keys stay on ${thisMac}.`}>
              {sshState.state.profiles.map((profile) => (
                <SSHRow key={profile.id} profile={profile} status={sshState.status(profile.id)} edit={() => setEditing({ profile, isNew: false })} />
              ))}
            </CardSection>
          ) : null}
          <Text as="div" size="caption" tone="secondary" className="settings-hint">
            Right-click a computer: Voice chat, Backups, Disconnect.
          </Text>
        </>
      ) : null}

      <Sheet className="settings-sheet" open={deviceStore !== null} onClose={() => setDevice(null)} width={460}>
        {deviceStore ? (
          <DeviceSheet
            store={deviceStore}
            ssh={sshState.isSSH(deviceStore.computer.id)}
            onPair={() => setPairing(deviceStore)}
            openPanel={(kind) => {
              setDevice(null)
              setPanel({ kind, store: deviceStore })
            }}
          />
        ) : null}
      </Sheet>
      <Sheet className="settings-sheet" open={pairing !== null} onClose={() => setPairing(null)} width={380}>
        {pairing ? (
          <>
            <ModalHeader title="Pair iPhone" />
            <PairingPanel store={pairing} />
          </>
        ) : null}
      </Sheet>
      <Sheet className="settings-sheet" open={editing !== null} onClose={() => setEditing(null)} width={460}>
        {editing ? <SSHProfileEditor key={editing.profile.id} profile={editing.profile} isNew={editing.isNew} /> : null}
      </Sheet>
      <Sheet className="settings-sheet" open={panel !== null} onClose={() => setPanel(null)}>
        {panel ? (
          <StoreContext.Provider value={panel.store}>
            {panel.kind === 'voice' ? <VoiceChatSettingsView /> : <BackupsSheet store={panel.store} />}
          </StoreContext.Provider>
        ) : null}
      </Sheet>
    </>
  )
}

/** This Mac or an SSH computer: its relay, pairing and devices card, then Voice chat and Backups. */
function DeviceSheet({ store, ssh, onPair, openPanel }: { store: BotStore; ssh: boolean; onPair: () => void; openPanel: (kind: 'voice' | 'backups') => void }) {
  return (
    <>
      <ModalHeader title={ssh ? `${store.hostName} · over SSH` : store.hostName} />
      <div className="settings-device-sheet">
        <ManagedComputerCard store={store} onPair={onPair} />
        <CardSection>
          <button className="settings-row settings-nav-row" onClick={() => openPanel('voice')}>
            <Icon name="waveform" size={13} color="var(--secondary)" />
            <span className="settings-grow">Voice chat</span>
            <Icon name="chevron.right" size={11} color="var(--secondary)" />
          </button>
          <button className="settings-row settings-nav-row" onClick={() => openPanel('backups')}>
            <Icon name="arrow.triangle.2.circlepath" size={13} color="var(--secondary)" />
            <span className="settings-grow">Backups</span>
            <Icon name="chevron.right" size={11} color="var(--secondary)" />
          </button>
        </CardSection>
      </div>
    </>
  )
}

function BackupsSheet({ store }: { store: BotStore }) {
  return (
    <div className="settings-sheet-body">
      <ModalHeader title={`Backups on ${store.hostName}`} />
      <div className="settings-devices-body">
        <BackupChains only={store.computer.id} />
      </div>
    </div>
  )
}

/** One computer: its badge, route and bots, the Show in Crew switch (or the way to ask for access), and its right-click menu. */
function ComputerRow({ row, shown, onShow, openPanel, onOpen }: {
  row: MacRow
  shown: boolean
  onShow: () => void
  openPanel: (kind: 'voice' | 'backups', store: BotStore) => void
  /** This Mac and SSH computers open their sheet from the row. */
  onOpen?: () => void
}) {
  const app = useApp()
  const macs = app.relayMacs
  const store = app.store(row.id)
  useModel(store ?? null)
  const menu = useContextMenu()
  const route = row.kind === 'relay' ? 'Cloudflare' : row.kind === 'ssh' ? 'SSH' : store?.cloud?.enabled && store.cloud.connected ? 'Wi-Fi · reach from anywhere on' : 'Wi-Fi'
  const bots = store ? `${store.roster.length} bot${store.roster.length === 1 ? '' : 's'} · ` : ''
  const sub = row.kind === 'relay' && !store ? `in your account · ${row.status.toLowerCase()}` : `${bots}${route}${row.online ? '' : ` · ${row.status.toLowerCase()}`}`
  const items = (): MenuItem[] => [
    ...(store && row.online
      ? [
          { title: 'Voice chat', icon: 'waveform', action: () => openPanel('voice', store) },
          { title: 'Backups', icon: 'arrow.triangle.2.circlepath', action: () => openPanel('backups', store) },
        ]
      : []),
    ...(row.kind === 'relay' && (row.action === 'disconnect' || row.action === 'forget')
      ? [{ title: row.action === 'forget' ? 'Forget' : 'Disconnect', icon: 'xmark.circle', destructive: true, divider: !!store && row.online, action: () => void macs.disconnect(row.id) }]
      : []),
  ]
  const hasMenu = items().length > 0
  const hasSwitch = store !== undefined && row.action !== 'forget'
  return (
    <div
      className={`settings-computer${onOpen ? ' settings-computer-open' : ''}`}
      onContextMenu={hasMenu ? menu.onContextMenu : undefined}
      role={onOpen ? 'button' : undefined}
      tabIndex={onOpen ? 0 : undefined}
      onClick={onOpen}
      onKeyDown={onOpen ? (e) => e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), onOpen()) : undefined}
    >
      <ComputerBadge computer={row.computer} size={28} />
      <VStack grow className="settings-stack">
        <Text tone="text" truncate>{row.name}{row.kind === 'this' ? ` · ${thisMac}` : ''}</Text>
        <Text size="caption" tone="secondary" truncate>{sub}</Text>
      </VStack>
      {row.code ? <Text size="headline" mono aria-label={`Digits to check: ${row.code}`}>{spaced(row.code)}</Text> : null}
      {row.action === 'connect' ? <Button kind="secondary" onClick={() => void macs.connect(row.id)}>Ask for access</Button> : null}
      {row.action === 'cancel' ? <Button kind="secondary" onClick={() => macs.cancel(row.id)}>Cancel</Button> : null}
      {row.action === 'forget' ? <Button kind="secondary" onClick={() => void macs.disconnect(row.id)}>Forget</Button> : null}
      {hasSwitch ? (
        <span onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
          <Switch className="bare" on={shown} onChange={onShow} label={`Show ${row.name} in Crew`} />
        </span>
      ) : null}
      {onOpen ? <Icon name="chevron.right" size={11} color="var(--secondary)" /> : null}
      <AnchoredMenu open={menu.open} onClose={menu.close} point={menu.point} items={items} />
    </div>
  )
}

/** This computer or an SSH computer: relay, account membership, pairing and authorized devices. */
function ManagedComputerCard({ store, onPair }: { store: BotStore; onPair: () => void }) {
  const app = useApp()
  const account = useAccount()
  useModel(store)
  const [devices, setDevices] = useState<AuthorizedDevice[] | null>(null)
  const [busy, setBusy] = useState(false)
  const [showDevices, setShowDevices] = useState(false)
  const [confirmRevoke, setConfirmRevoke] = useState<AuthorizedDevice | null>(null)
  const [confirmUnclaim, setConfirmUnclaim] = useState(false)
  const [confirmAutoApproval, setConfirmAutoApproval] = useState(false)
  const [confirmOwnerChange, setConfirmOwnerChange] = useState(false)
  const cloud = store.cloud
  const autoApproval = cloud?.approval === 'auto'
  const online = store.connection.kind === 'online'
  const name = store.hostName
  const requests = store.accessRequests.length

  const run = (work: () => Promise<void>) => {
    setBusy(true)
    void work().finally(() => setBusy(false))
  }

  const loadDevices = useCallback(async () => {
    const client = liveClient(store)
    if (!client) return
    try {
      setDevices((await client.call<{ devices: AuthorizedDevice[] }>('devices')).devices)
    } catch {}
  }, [store])

  useEffect(() => void loadDevices(), [loadDevices, online, store.accessRequests.length])

  const setApproval = (approval: AccountApproval) =>
    run(async () => {
      try {
        await (await store.ready()).call('setApproval', { approval })
      } catch (e) {
        store.setError(errorText(e))
      }
    })

  const cloudLine = !cloud?.enabled
    ? 'Off: only Wi-Fi and Tailscale reach it, nothing goes through Cloudflare.'
    : (cloud.lastError ?? (cloud.connected ? 'Relay connected: reachable from anywhere through Cloudflare (encrypted).' : 'Connecting to Cloudflare…'))

  const owner = cloud?.owner ?? null
  const accountLine = (
    <HStack gap={8} className="settings-row">
      <Icon name={owner ? 'person.crop.circle.badge.checkmark' : 'person.crop.circle.badge.plus'} size={13} color="var(--secondary)" />
      <Text size="callout" grow>
        {owner
          ? owner.userId === account.user?.userId
            ? 'In your account'
            : `In ${owner.email ?? 'another'} account`
          : account.isSignedIn
            ? 'Not in your account'
            : 'Sign in to add it to your account'}
      </Text>
      {owner ? (
        <Button kind="secondary" onClick={() => setConfirmUnclaim(true)} disabled={busy || !online}>
          Remove from account
        </Button>
      ) : account.isSignedIn ? (
        <Button kind="secondary" onClick={() => run(async () => void (await app.cloud.claim(store)))} disabled={busy || !online}>
          {busy ? 'Adding…' : 'Add to account'}
        </Button>
      ) : null}
    </HStack>
  )

  const change = cloud?.ownerChange ?? null
  const newOwner = change?.owner ?? null
  const changeText = newOwner ? `Kith's cloud says ${name} now belongs to ${newOwner.email ?? newOwner.userId}.` : `Kith's cloud says ${name} has left its account.`
  const ownerChangeLine = change ? (
    <HStack key="ownerChange" gap={8} className="settings-row">
      <VStack grow className="settings-stack">
        <span>Owner change waiting</span>
        <Text size="caption" tone="attention">{changeText} Nothing that depends on the owner changes until you confirm it here.</Text>
      </VStack>
      <Button kind="secondary" onClick={() => setConfirmOwnerChange(true)} disabled={busy || !online}>Review</Button>
    </HStack>
  ) : null
  // Pinned from the cloud's first answer (this Mac knew no account of its own): shown until confirmed here.
  const fromCloudLine =
    !change && owner && cloud?.ownerFromCloud ? (
      <HStack key="ownerFromCloud" gap={8} className="settings-row">
        <VStack grow className="settings-stack">
          <span>Owner set from the cloud</span>
          <Text size="caption" tone="attention">
            {`${name} knew no account of its own when it took ${owner.email ?? owner.userId} as its owner from Kith's cloud. Confirm it if that's your account.`}
          </Text>
        </VStack>
        <Button kind="secondary" onClick={() => confirmOwner(owner.userId)} disabled={busy || !online}>Confirm</Button>
      </HStack>
    ) : null
  const confirmOwner = (userId: string | null) =>
    run(async () => {
      try {
        await (await store.ready()).call('confirmOwner', { userId }, 30_000)
        await app.cloud.refresh()
      } catch (e) {
        store.setError(errorText(e))
      }
    })

  const deviceNames = devices?.map((d) => d.name).join(', ')
  return (
    <>
      <CardSection>
        {[
          <Switch key="cloud" on={cloud?.enabled ?? false} disabled={busy || !online} onChange={(on) => run(() => app.cloud.setCloud(store, on))}>
            <span className="settings-stack">
              <span>Reach from anywhere</span>
              <Text size="caption" tone={cloud?.lastError ? 'attention' : 'secondary'}>{cloudLine}</Text>
            </span>
          </Switch>,
          <div key="pair" className="settings-row">
            <VStack grow className="settings-stack">
              <span>Pair iPhone</span>
              <Text size="caption" tone="secondary">show a one-time code</Text>
            </VStack>
            <Button kind="secondary" disabled={!online} onClick={onPair}>Show code</Button>
          </div>,
          <button key="devices" className="settings-row settings-nav-row" onClick={() => setShowDevices(true)}>
            <VStack grow className="settings-stack">
              <span>Devices that can use {name}</span>
              <Text size="caption" tone="secondary" truncate>
                {devices === null ? 'Loading…' : `${deviceNames || 'None yet'}${requests > 0 ? ` · ${requests} request${requests === 1 ? '' : 's'}` : ''}`}
              </Text>
            </VStack>
            {requests > 0 ? <span className="settings-count">{requests}</span> : null}
            <Icon name="chevron.right" size={11} color="var(--secondary)" />
          </button>,
          accountLine,
          ...(ownerChangeLine ? [ownerChangeLine] : []),
          ...(fromCloudLine ? [fromCloudLine] : []),
          ...(owner
            ? [
                <Switch
                  key="approval"
                  on={autoApproval}
                  disabled={busy || !online}
                  onChange={(on) => (on ? setConfirmAutoApproval(true) : setApproval('code'))}
                >
                  <span className="settings-stack settings-appear">
                    <span>Skip the 6-digit check</span>
                    <Text size="caption" tone={autoApproval ? 'attention' : 'secondary'}>
                      {autoApproval ? 'Devices on your account get in on their own.' : 'Each new device shows a code you approve here.'}
                    </Text>
                  </span>
                </Switch>,
              ]
            : []),
        ]}
      </CardSection>

      <Sheet className="settings-sheet" open={showDevices} onClose={() => setShowDevices(false)} width={460}>
        <ModalHeader title={`Devices that can use ${name}`} />
        <div className="settings-devices-body">
          <CardSection>
            {devices === null
              ? [<Spinner key="loading" />]
              : [
                  ...(requests > 0
                    ? [
                        <div key="requests" className="settings-row">
                          <Text grow>{requests} access request{requests === 1 ? '' : 's'} waiting</Text>
                          <Button onClick={() => { app.reviewApprovals(); setShowDevices(false) }}>Review</Button>
                        </div>,
                      ]
                    : []),
                  ...(devices.length === 0
                    ? [<Text key="none" tone="secondary">None yet. Pair an iPhone, or approve one from your account.</Text>]
                    : []),
                  ...devices.map((device) => (
                    <HStack key={device.key} gap={8} className="settings-row settings-appear">
                      <IconSlot width={18} tone="secondary">
                        <Icon name={deviceIcon(device.platform)} size={13} />
                      </IconSlot>
                      <VStack grow className="settings-stack">
                        <span>{device.name}</span>
                        <Text size="caption" tone="tertiary">{deviceDetail(device)}</Text>
                      </VStack>
                      <Button kind="secondary" onClick={() => setConfirmRevoke(device)}>Revoke</Button>
                    </HStack>
                  )),
                ]}
          </CardSection>
        </div>
      </Sheet>
      <Dialog
        open={confirmRevoke !== null}
        title={`Revoke ${confirmRevoke?.name ?? 'device'}?`}
        message="It disconnects right away and has to pair or ask again."
        actions={
          confirmRevoke
            ? [{
                title: 'Revoke access',
                destructive: true,
                action: () =>
                  run(async () => {
                    try {
                      await (await store.ready()).call('revokeDevice', { key: confirmRevoke.key })
                    } catch (e) {
                      store.setError(errorText(e))
                    }
                    await loadDevices()
                  }),
              }]
            : []
        }
        onClose={() => setConfirmRevoke(null)}
      />
      <Dialog
        open={confirmAutoApproval}
        title="Let account devices in without a code?"
        message={`Any device signed in to your account gets full control of ${name}: its files, terminals and bots, with no check here. If someone gets into your account, or Kith's cloud is ever compromised, they could add their own device and you'd have no chance to stop it. Devices paired with a QR code aren't affected.`}
        actions={[{ title: 'Skip the check', destructive: true, action: () => setApproval('auto') }]}
        onClose={() => setConfirmAutoApproval(false)}
      />
      <Dialog
        open={confirmUnclaim}
        title={`Remove ${name} from the account?`}
        message="Devices that were approved through the account lose access. Devices paired with a QR code keep it."
        actions={[{ title: 'Remove from account', destructive: true, action: () => run(() => app.cloud.unclaim(store)) }]}
        onClose={() => setConfirmUnclaim(false)}
      />
      <Dialog
        open={confirmOwnerChange}
        title="Accept the new owner?"
        message={`${changeText} Accept only if you made this change: the new account's devices can then be let in, and devices approved through ${owner?.email ?? 'the old account'} lose access.`}
        actions={[{ title: newOwner ? 'Accept new owner' : 'Accept leaving', destructive: true, action: () => confirmOwner(newOwner?.userId ?? null) }]}
        onClose={() => setConfirmOwnerChange(false)}
      />
    </>
  )
}

function deviceDetail(device: AuthorizedDevice) {
  const parts = [device.source === 'account' ? 'Approved through the account' : 'Paired with a QR code']
  if (device.connected) parts.push('connected')
  else if (device.lastSeenAt) parts.push(`seen ${relativeDay(new Date(device.lastSeenAt))}`)
  return parts.join(' · ')
}

/** A one-time pairing QR from a computer this app manages. */
function PairingPanel({ store }: { store: BotStore }) {
  useModel(store)
  const load = useCallback(async (): Promise<PairingSource> => {
    const client = liveClient(store)
    if (!client) throw new Error(`${store.hostName} isn't connected.`)
    try {
      return { info: await client.call<PairingInfo>('pairing'), name: store.hostName, cloud: store.cloud }
    } catch (e) {
      throw new Error(errorText(e))
    }
  }, [store])
  return <PairingCode load={load} cloud={store.cloud} />
}
// MARK: SSH

/** Add or change an SSH computer. Only fields ssh takes as separate arguments; nothing goes through a shell. */
function SSHProfileEditor({ profile: initial, isNew }: { profile: SSHProfile; isNew: boolean }) {
  const dismiss = useDismiss()
  const [profile, setProfile] = useState(initial)
  const [port, setPort] = useState(initial.port?.toString() ?? '')
  const [remotePort, setRemotePort] = useState(String(initial.remotePort))
  const [user, setUser] = useState(initial.user ?? '')
  const [problem, setProblem] = useState<string | null>(null)
  const bridge = window.kith.ssh

  const save = async () => {
    const isInt = (s: string) => /^-?\d+$/.test(s)
    const portText = port.trim()
    if (portText && !isInt(portText)) return setProblem('The SSH port must be a number.')
    const remote = remotePort.trim()
    if (!isInt(remote)) return setProblem('The Kith port must be a number.')
    const u = user.trim()
    const p: SSHProfile = {
      ...profile,
      host: profile.host.trim(),
      name: profile.name.trim(),
      user: u || null,
      port: portText ? Number(portText) : null,
      remotePort: Number(remote),
    }
    const found = await bridge.save(p)
    if (found) return setProblem(found)
    dismiss()
  }

  const field = (label: string, value: string, set: (v: string) => void, prompt: string) => (
    <HStack gap={12} className="settings-row">
      <Text tone="text">{label}</Text>
      <input className="settings-field" aria-label={label} placeholder={prompt} value={value} spellCheck={false} onChange={(e) => set(e.target.value)} />
    </HStack>
  )

  return (
    // Return in a field saves (the default action).
    <VStack className="settings-ssh-editor" onKeyDown={(e) => e.key === 'Enter' && e.target instanceof HTMLInputElement && void save()}>
      <ModalHeader title={isNew ? 'Add SSH computer' : 'Edit SSH computer'} trailing={<IconButton title={isNew ? 'Add' : 'Save'} icon="checkmark" onClick={() => void save()} />} />
      <VStack gap={16} className="settings-ssh-editor-body">
        <CardSection>
          {field('Name', profile.name, (name) => setProfile({ ...profile, name }), 'Optional')}
          {field('Host', profile.host, (host) => setProfile({ ...profile, host }), 'SSH alias or hostname')}
          {field('User', user, setUser, 'From SSH config')}
          {field('SSH port', port, setPort, 'From SSH config')}
          <div className="settings-row">
            <span>Key</span>
            <Spacer />
            <Text tone="secondary">{profile.identityFile?.split('/').pop() ?? 'ssh-agent / SSH config'}</Text>
            <IconButton
              title="Choose key file"
              icon="key"
              onClick={() => void bridge.chooseKey().then((path) => path && setProfile((p) => ({ ...p, identityFile: path })))}
            />
            {profile.identityFile ? (
              <HStack as="span" align="stretch" className="settings-appear">
                <IconButton title="Use ssh-agent" icon="xmark.circle" onClick={() => setProfile({ ...profile, identityFile: null })} />
              </HStack>
            ) : null}
          </div>
          {field('Kith port', remotePort, setRemotePort, '')}
        </CardSection>
        <Text size="caption" tone="secondary">
          Kith opens an SSH tunnel to kith-serve on that computer's loopback. It uses your SSH config and ssh-agent; agent forwarding stays off.
        </Text>
        <Reveal show={problem !== null}>
          <Text size="caption" tone="danger">{problem}</Text>
        </Reveal>
      </VStack>
    </VStack>
  )
}
