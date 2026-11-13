import { useEffect, useRef, useState } from 'react'
import { CardForm, CardSection, IconButton, Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { ModalHeader, SheetPage, useDismiss } from '../../components/Overlay'
import { VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { canInstall, installName, type InstallPreview, type MarketBot } from '@shared/kith-tools'
import { errorText } from './market-models'
import { ErrorText, Field, ItemGrid, ItemRow } from './pieces'
import { useKithTools } from './kith-tools'

/**
 * "Which bot?": the owner's kith agents, each with a button to put the connector or skill on it or take it off.
 * The CLI does the change (`kith <agent> connect|skill add`); a row shows the CLI's answer read back.
 */
export function WhichBotsSheet({ kind, name, title, note }: { kind: 'connector' | 'skill'; name: string; title: string; note?: string }) {
  const store = useStore()
  const kith = useKithTools(store)
  const dismiss = useDismiss()
  const [error, setError] = useState<string | null>(null)
  const holders = kind === 'connector' ? kith.connectorHolders(name) : (kith.skillHolders().get(name) ?? [])

  const toggle = async (bot: (typeof kith.bots)[number], has: boolean) => {
    setError(null)
    try {
      if (kind === 'connector') await (has ? kith.disconnect(bot, name) : kith.connect(bot, name))
      else await (has ? kith.removeSkill(bot, name) : kith.addSkill(bot, name))
    } catch (e) {
      setError(errorText(e))
    }
  }

  return (
    <SheetPage>
      <ModalHeader title={title} trailing={<IconButton title="Done" icon="checkmark" onClick={dismiss} />} />
      <CardForm>
        <CardSection title="Which bot?" footer={note ?? (kind === 'connector' ? 'A bot only uses the connectors you put on it.' : 'A bot only reads the skills you put on it.')}>
          {kith.bots.length === 0 ? <Text tone="secondary">No bots yet. Create one, or install one from Bots.</Text> : null}
          <ItemGrid>
            {kith.bots.map((bot) => {
              const has = holders.includes(bot.hqAgent!)
              const busy = kith.isPending(bot, kind, name)
              return (
                <ItemRow
                  key={bot.id}
                  title={bot.name}
                  subtitle={has ? 'Has it' : bot.hqAgent!}
                  icon={<Icon name="person.crop.circle" size={20} />}
                  trailing={busy ? <Spinner /> : (
                    <button className="capsule-action" disabled={busy} onClick={() => void toggle(bot, has)}>{has ? 'Remove' : 'Add'}</button>
                  )}
                />
              )
            })}
          </ItemGrid>
        </CardSection>
        {error ? <CardSection><ErrorText text={error} /></CardSection> : null}
      </CardForm>
    </SheetPage>
  )
}

/** What installing a shared bot would do here, shown before anything is kept; Install keeps it. */
export function BotPreviewSheet({ bot, done }: { bot: MarketBot; done: () => void }) {
  const store = useStore()
  const dismiss = useDismiss()
  const [preview, setPreview] = useState<InstallPreview | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [typed, setTyped] = useState('')
  const [installing, setInstalling] = useState(false)
  const started = useRef(false)

  useEffect(() => {
    if (started.current) return
    started.current = true
    void (async () => {
      try {
        setPreview(await (await store.ready()).kithInstallPreview(bot.url || bot.repo))
      } catch (e) {
        setError(errorText(e))
      }
    })()
  }, [])

  const install = async () => {
    if (!preview || installing) return
    setInstalling(true)
    setError(null)
    try {
      await (await store.ready()).kithInstall(bot.url || bot.repo, installName(preview, typed))
      // The host finds new agents on a timer; ask now so the roster has the bot when the sheet closes (its events carry the bots).
      await (await store.ready()).call('syncHqBots', {}, 60_000).catch(() => {})
      done()
      dismiss()
    } catch (e) {
      setError(errorText(e))
      setInstalling(false)
    }
  }

  const list = (items: string[]) => items.map((t) => <Text key={t} size="footnote" tone="secondary">{t}</Text>)
  return (
    <SheetPage>
      <ModalHeader
        title={bot.name}
        trailing={installing ? <Spinner /> : <IconButton title="Install" icon="plus" disabled={!preview || !canInstall(preview, typed)} onClick={() => void install()} />}
      />
      <CardForm>
        <CardSection>
          <VStack gap={6}>
            <Text tone="secondary">{preview?.description || bot.description || bot.repo}</Text>
            <Text size="caption" tone="secondary">{bot.repo} · {bot.stars} {bot.stars === 1 ? 'star' : 'stars'}</Text>
          </VStack>
        </CardSection>
        {!preview && !error ? <CardSection><Spinner /></CardSection> : null}
        {preview ? (
          <>
            {preview.nameTaken ? (
              <CardSection title="Name" footer={`You already have a bot called ${preview.name}. Install it under another name.`}>
                <Field label="Install as" value={typed} placeholder={preview.suggestedName} onChange={setTyped} />
              </CardSection>
            ) : null}
            {preview.needs.length ? (
              <CardSection title="Services it uses" footer="Connect them to it after installing, in Extensions.">
                {preview.needs.map((n) => (
                  <Text key={n.service} size="footnote">
                    {n.service}: {!n.known ? 'not on this Mac' : n.granted ? 'ready' : n.hasProfile ? 'set up on this Mac' : 'needs signing in or a key'}
                  </Text>
                ))}
              </CardSection>
            ) : null}
            {preview.requires.length ? (
              <CardSection title="Programs it runs">
                {preview.requires.map((r) => (
                  <Text key={r.program} size="footnote">{r.program}: {r.present ? 'installed' : 'missing on this Mac'}{r.why ? ` (${r.why})` : ''}</Text>
                ))}
              </CardSection>
            ) : null}
            {preview.skills.own.length || preview.skills.linked.length ? (
              <CardSection title="Skills">{list([...preview.skills.own, ...preview.skills.linked.map((s) => `${s} (from GitHub)`)])}</CardSection>
            ) : null}
            <CardSection title="Permissions" footer={preview.permissions.mode ? `Starts in ${preview.permissions.mode} mode.` : undefined}>
              {preview.permissions.allow.length ? list(['Always allowed:', ...preview.permissions.allow]) : <Text size="footnote" tone="secondary">Nothing allowed ahead of time.</Text>}
              {preview.permissions.hooks.length ? list(['Runs on its own:', ...preview.permissions.hooks]) : null}
              {preview.permissions.mcp.length ? list(['Tool servers:', ...preview.permissions.mcp]) : null}
              {preview.permissions.tools.length ? list(['Its own tools:', ...preview.permissions.tools]) : null}
            </CardSection>
            {preview.warnings.length ? <CardSection title="Warnings">{preview.warnings.map((w) => <Text key={w} size="footnote" tone="danger">{w}</Text>)}</CardSection> : null}
          </>
        ) : null}
        {error ? <CardSection><ErrorText text={error} /></CardSection> : null}
      </CardForm>
    </SheetPage>
  )
}
