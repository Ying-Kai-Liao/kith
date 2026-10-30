import { useEffect, useState } from 'react'
import { isGroup, type Bot } from '@shared/models'
import { addableBots } from '@shared/room-join'
import { BotAvatar } from '../../components/Avatar'
import { Button } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { ModalHeader, SheetPage, useDismiss } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { BotGuestAvatar } from '../../components/kith'
import { useModel } from '../../lib/observable'
import { botGuestsFor } from '../../store/bot-guests-store'
import { useStore } from '../../store/context'
import { errorText } from '../bots/drafts'
import { BotTag } from './Tags'
import './conversation.css'

type Pick = { kind: 'agent'; bot: Bot } | { kind: 'bot'; id: string; name: string; provider: string }

const providerWord = (provider: string) => (provider ? provider.charAt(0).toUpperCase() + provider.slice(1) : 'it')

/**
 * Room info › Agents and bots › Add, as a window sheet. The owner's agents and bots that aren't in the room yet; a bot
 * that can't join says why. Picking a bot asks the one question that matters (it reads this room, outside this Mac)
 * in a card, not an alert; adding goes through the same members edit `kith group add-bot` uses.
 */
export function AddToRoomSheet({ group }: { group: Bot }) {
  const store = useStore()
  const dismiss = useDismiss()
  const guests = useModel(botGuestsFor(store))
  const [picked, setPicked] = useState<Pick | null>(null)
  const [adding, setAdding] = useState(false)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => void guests.load(), [guests])

  const agents = store.roster.filter((b) => !isGroup(b) && !b.botGuest && !group.members.includes(b.id))
  const bots = addableBots(guests.bots, group.members)
  const name = picked ? (picked.kind === 'agent' ? picked.bot.name : picked.name) : ''

  const add = () => {
    if (!picked || adding) return
    setAdding(true)
    setError(null)
    const id = picked.kind === 'agent' ? picked.bot.id : picked.id
    void store
      .updateGroup({ id: group.id, kind: 'group', name: group.name, description: group.description, members: [...group.members, id], pinned: group.pinned })
      .then(() => dismiss())
      .catch((e: unknown) => setError(errorText(e)))
      .finally(() => setAdding(false))
  }

  return (
    <SheetPage>
      <ModalHeader title={`Add to ${group.name}`} />
      <div className="add-room-body">
        {agents.length ? (
          <>
            <Text as="div" size="caption" weight="semibold" tone="secondary" className="add-room-heading">YOUR AGENTS</Text>
            <div className="add-room-list">
              {agents.map((bot) => (
                <button key={bot.id} className="press add-room-row" aria-pressed={picked?.kind === 'agent' && picked.bot.id === bot.id} onClick={() => setPicked({ kind: 'agent', bot })}>
                  <BotAvatar bot={bot} size={32} animated={false} />
                  <span className="add-room-name">{bot.name}</span>
                  <Radio on={picked?.kind === 'agent' && picked.bot.id === bot.id} />
                </button>
              ))}
            </div>
          </>
        ) : null}
        {bots.length ? (
          <>
            <Text as="div" size="caption" weight="semibold" tone="secondary" className="add-room-heading">YOUR BOTS</Text>
            <div className="add-room-list">
              {bots.map(({ bot, why }) => {
                const on = picked?.kind === 'bot' && picked.id === bot.botId
                return (
                  <button
                    key={bot.botId}
                    className={`press add-room-row ${why ? 'cannot' : ''}`}
                    disabled={!!why}
                    aria-pressed={on}
                    onClick={() => setPicked({ kind: 'bot', id: bot.botId, name: bot.name, provider: bot.provider })}
                  >
                    <BotGuestAvatar provider={bot.provider} size={32} />
                    <span className="add-room-name">
                      <span><span className="bot-name">{bot.name}</span><BotTag /></span>
                      <Text as="span" size="footnote" tone="secondary">{why ?? `Your bot · on ${providerWord(bot.provider)}`}</Text>
                    </span>
                    {why ? null : <Radio on={on} />}
                  </button>
                )
              })}
            </div>
          </>
        ) : null}
        {!agents.length && !bots.length ? <Text as="div" size="subheadline" tone="secondary" className="add-room-empty">Everyone you have is in this room already.</Text> : null}
      </div>
        {picked ? (
          <div className="add-room-card">
            {picked.kind === 'bot' ? (
              <>
                <div className="add-room-card-title">
                  <BotGuestAvatar provider={picked.provider} size={30} />
                  <Text size="body" weight="semibold" tone="text">{picked.name} will see this room from now on.</Text>
                </div>
                <Text as="div" size="subheadline" tone="secondary">It reads what is said here and answers on its turn or when someone @-mentions it. Its replies use the turns you give.</Text>
                <div className="add-room-fact">
                  <Icon name="square.and.arrow.up" size={12} color="var(--secondary)" />
                  <Text size="subheadline" tone="text">What it reads goes to {providerWord(picked.provider)}, outside this Mac.</Text>
                </div>
              </>
            ) : (
              <div className="add-room-card-title">
                <BotAvatar bot={picked.bot} size={30} animated={false} />
                <Text size="body" weight="semibold" tone="text">{picked.bot.name} joins this room.</Text>
              </div>
            )}
            {error ? <Text as="div" size="footnote" tone="danger">{error}</Text> : null}
            <div className="add-room-actions">
              <Button onClick={add} disabled={adding}>Add {name}</Button>
              <Button kind="secondary" onClick={dismiss}>Cancel</Button>
            </div>
          </div>
        ) : null}
    </SheetPage>
  )
}

function Radio({ on }: { on: boolean }) {
  return <span className={`add-room-radio ${on ? 'on' : ''}`} aria-hidden>{on ? <Icon name="checkmark.circle.fill" size={20} color="var(--kith-ink)" /> : null}</span>
}
