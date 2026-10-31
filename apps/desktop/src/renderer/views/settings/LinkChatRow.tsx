import { useEffect, useState } from 'react'
import { isUnsupportedHost } from '@shared/kith-info'
import { Button, Switch } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import { useApp } from '../../store/context'
import type { LinkCode } from '../../store/bot-store'
import { errorText } from './parts'

/** "Link Telegram or Discord": a one-time code that ties a chat account to your Kith person. */
export function LinkChatRow() {
  const app = useApp()
  const local = useModel(app.local)
  const [code, setCode] = useState<LinkCode | null>(null)
  const [busy, setBusy] = useState(false)
  const [unsupported, setUnsupported] = useState(false)
  const [copied, setCopied] = useState(false)
  const [error, setError] = useState<string | null>(null)
  if (!local || local.connection.kind !== 'online' || unsupported) return null

  const request = async () => {
    if (busy) return
    setBusy(true)
    setError(null)
    setCopied(false)
    try {
      setCode(await local.linkCode())
    } catch (e) {
      // An older host doesn't know the method: the row goes away rather than failing.
      if (isUnsupportedHost(e)) setUnsupported(true)
      else setError(errorText(e))
    } finally {
      setBusy(false)
    }
  }
  const copy = () => {
    if (!code) return
    void navigator.clipboard.writeText(`/link ${code.code}`).then(() => setCopied(true), () => setError("Couldn't copy the code."))
  }
  const minutes = code ? Math.max(1, Math.round((code.expiresAt - Date.now()) / 60_000)) : 0

  return (
    <VStack gap={8}>
      <div className="settings-row">
        <Icon name="link" size={26} color="var(--secondary)" />
        <VStack grow className="settings-stack">
          <Text truncate>Link Telegram or Discord</Text>
          <Text size="caption" tone="secondary">Chat with your bots from your own Telegram or Discord account</Text>
        </VStack>
        <Button kind="secondary" onClick={() => void request()} disabled={busy}>
          {code ? 'New code' : 'Link'}
        </Button>
      </div>
      {code ? (
        <VStack gap={6} className="settings-link-code">
          <HStack gap={10} align="center">
            <Text as="div" size={28} weight="semibold" className="settings-link-code-value">{code.code}</Text>
            <Button kind="secondary" onClick={copy}>{copied ? 'Copied' : 'Copy'}</Button>
          </HStack>
          <Text size="footnote" tone="secondary">
            Send /link {code.code} to your Kith bot in Telegram or Discord. Expires in about {minutes} min.
          </Text>
        </VStack>
      ) : null}
      {error ? <Text size="footnote" tone="attention">{error}</Text> : null}
    </VStack>
  )
}

/** The owner's switch for reply cards: Jev sorts a finished reply that reads like a result into a card (off until turned on). */
export function ReplyCardsRow() {
  const app = useApp()
  const local = useModel(app.local)
  const online = local?.connection.kind === 'online'
  const [on, setOn] = useState<boolean | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    if (!local || !online) return
    let live = true
    // A host that doesn't know the method, or won't answer this person, leaves the row hidden.
    local.replyCards().then((v) => live && setOn(v), () => undefined)
    return () => {
      live = false
    }
  }, [local, online])
  if (!local || !online || on === null) return null

  const set = (next: boolean) => {
    setOn(next)
    setError(null)
    local.replyCards(next).then(setOn, (e: unknown) => {
      setOn(!next)
      setError(errorText(e))
    })
  }
  return (
    <VStack gap={8}>
      <div className="settings-row">
        <Icon name="doc.text" size={26} color="var(--secondary)" />
        <VStack grow className="settings-stack">
          <Text truncate>Result cards for replies</Text>
          <Text size="caption" tone="secondary">Lets Jev turn a reply that reads like a result into a card, using the reply's own words. The reply stays one tap away.</Text>
        </VStack>
        <Switch on={on} onChange={set} label="Result cards for replies" />
      </div>
      {error ? <Text size="footnote" tone="attention">{error}</Text> : null}
    </VStack>
  )
}

/** Whether a Schedule card's words also go to your Telegram or Discord chat with the agent. The card stays in the app either way. */
export function ScheduleMessagesRow() {
  const app = useApp()
  const local = useModel(app.local)
  const online = local?.connection.kind === 'online'
  const [on, setOn] = useState<boolean | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    if (!local || !online) return
    let live = true
    // An older host doesn't know the method: the row stays hidden.
    local.scheduleMessages().then((v) => live && setOn(v), () => undefined)
    return () => {
      live = false
    }
  }, [local, online])
  if (!local || !online || on === null) return null

  const set = (next: boolean) => {
    setOn(next)
    setError(null)
    local.scheduleMessages(next).then(setOn, (e: unknown) => {
      setOn(!next)
      setError(errorText(e))
    })
  }
  return (
    <VStack gap={8}>
      <div className="settings-row">
        <Icon name="calendar" size={26} color="var(--secondary)" />
        <VStack grow className="settings-stack">
          <Text truncate>Schedule changes in Telegram or Discord</Text>
          <Text size="caption" tone="secondary">Tell me there too when an agent adds, changes or removes a scheduled job</Text>
        </VStack>
        <Switch on={on} onChange={set} label="Schedule changes in Telegram or Discord" />
      </div>
      {error ? <Text size="footnote" tone="attention">{error}</Text> : null}
    </VStack>
  )
}
