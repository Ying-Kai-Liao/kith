import { useEffect } from 'react'
import { CODE_HINT, type JoinStep } from '@shared/invite-join'
import { Button, Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import { useAccount } from '../../store/account'
import { useApp } from '../../store/context'
import { inviteJoin } from '../../store/invite-join'
import { spaced } from '../../store/mac-list'

const progress: Record<JoinStep, string> = {
  signingIn: 'Sign in to join',
  joining: 'Joining…',
  asking: 'Asking the Mac for access…',
  connecting: 'Connecting to the Mac…',
  proving: 'Checking your invite…',
  done: "You're in",
  waiting: 'Waiting for the Mac',
  failed: "Couldn't join",
}

/** The invitee's side of `kith://invite?...`: sign in, join, ask the Mac, show the invite. */
export function InviteJoinSheet() {
  const join = useModel(inviteJoin)
  const account = useAccount()
  const app = useApp()
  useEffect(() => {
    inviteJoin.adopt = (id) => app.relayMacs.adopt(id)
    return () => void (inviteJoin.adopt = null)
  }, [app])
  // A link that arrived while the app was starting.
  useEffect(() => void inviteJoin.start(), [])
  useEffect(() => window.kith.app.onCommand((c) => c.kind === 'joinInvite' && void inviteJoin.start()), [])
  const state = join.state
  const busy = state !== null && ['joining', 'asking', 'connecting', 'proving'].includes(state.step)
  return (
    <Sheet className="settings-sheet" open={join.link !== null} onClose={() => inviteJoin.dismiss()} width={420}>
      <SheetPage>
        <ModalHeader title="Join a Mac" />
        <VStack align="center" gap={14} className="settings-invite-body">
          <HStack gap={8} align="center">
            {busy ? <Spinner /> : <Icon name={state?.step === 'done' ? 'checkmark.circle' : state?.step === 'failed' ? 'exclamationmark.triangle' : 'person.crop.circle'} size={22} color="var(--secondary)" />}
            <Text size="callout" weight="semibold">{state ? progress[state.step] : progress.signingIn}</Text>
          </HStack>
          {state?.message ? <Text size="callout" tone={state.step === 'failed' ? 'danger' : 'secondary'} className="settings-centered">{state.message}</Text> : null}
          {state?.step === 'asking' && state.code ? (
            <Text size="callout" tone="secondary" className="settings-centered">
              Your code: <Text mono weight="semibold" tone="text">{spaced(state.code)}</Text>. {CODE_HINT}
            </Text>
          ) : null}
          {!state || state.step === 'signingIn' ? (
            <>
              <Text size="callout" tone="secondary" className="settings-centered">Sign in with the account you want to use on this Mac.</Text>
              <HStack gap={8}>
                {(['apple', 'google', 'github'] as const).map((p) => (
                  <Button key={p} kind="secondary" onClick={() => void account.signIn(p)} disabled={account.isBusy}>{p === 'github' ? 'GitHub' : p[0]!.toUpperCase() + p.slice(1)}</Button>
                ))}
              </HStack>
            </>
          ) : null}
          {state?.step === 'waiting' ? <Button kind="secondary" onClick={() => void inviteJoin.run()}>Try again</Button> : null}
          {state?.step === 'done' || state?.step === 'failed' ? <Button onClick={() => inviteJoin.dismiss()}>Close</Button> : null}
          <Text size="footnote" tone="secondary" className="settings-centered">
            A link works on one device at a time. To use another device, open the link on it too.
          </Text>
        </VStack>
      </SheetPage>
    </Sheet>
  )
}
