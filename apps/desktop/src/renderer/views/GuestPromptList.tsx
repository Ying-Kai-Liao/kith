import { alwaysLabel, cardHeading, cardQuestion, type BotGuest } from '@shared/bot-guests'
import type { GuestPrompt } from '@shared/mac-role'
import { limitOf } from '@shared/usage-limits'
import { Spinner } from '../components/Controls'
import { BotGuestAvatar, DecisionCard, NEUTRAL_PAIR } from '../components/kith'
import { pairOf } from '../components/kith/pair.ts'
import { VStack } from '../components/Stack'
import { Text } from '../components/Text'
import { useModel } from '../lib/observable'
import { botGuestsFor } from '../store/bot-guests-store'
import type { BotStore } from '../store/bot-store'
import { LimitCard } from './thread/LimitCard'
import { optionLabel, sortedOptions } from './thread/permission-options.ts'
import { agentBot } from './usage/SpendSection'

/**
 * "Needs your OK": what other people's agents on this Mac ask, for the owner to answer. The data and the answering are
 * the store's (`guestPrompts`, `respond`); this is only their layout, so it can be restyled without touching them.
 */
export function GuestPromptList({ store }: { store: BotStore }) {
  useModel(store)
  const guests = botGuestsFor(store)
  useModel(guests)
  const prompts = store.guestPrompts
  const bots = guests.cards
  if (!prompts.length && !bots.length) return null
  return (
    <VStack gap={8} className="guest-prompts" aria-label="Needs your OK">
      <Text size="caption" weight="semibold" tone="secondary">Needs your OK</Text>
      {bots.map(({ bot, outcome }) => <BotRequestCard key={bot.botId} bot={bot} outcome={outcome} busy={guests.working.has(bot.botId)} decide={(d, always) => void guests.decide(bot, d, always)} />)}
      {prompts.map((p) => limitOf(p.entry) ? (
        // A person at their budget: the owner's card, in the colours of the agent they were talking to.
        <LimitCard
          key={p.entryId}
          entry={p.entry}
          pair={pairOf(agentBot(store, p.agent)?.avatarColor)}
          answering={store.answering.get(p.entryId) ?? null}
          respond={(option) => store.respond(p.entry, option)}
          personName={p.personName}
        />
      ) : (
        <GuestPromptRow key={p.entryId} prompt={p} answering={store.answering.get(p.entryId) ?? null} respond={(option) => store.respond(p.entry, option)} />
      ))}
    </VStack>
  )
}

function GuestPromptRow({ prompt, answering, respond }: { prompt: GuestPrompt; answering: string | null; respond: (option: string) => void }) {
  return (
    <VStack gap={4} className="guest-prompt">
      <Text size="callout" weight="medium" truncate>{`${prompt.personName} · ${prompt.agent}`}</Text>
      <Text size="caption" tone="secondary" mono lines={3}>{prompt.title}</Text>
      <div className="permission-card-options">
        {sortedOptions(prompt.options).map((o) => (
          <button
            key={o.optionId}
            className={answering === null || answering === o.optionId ? 'permission-option' : 'permission-option dimmed'}
            disabled={answering !== null}
            onClick={() => respond(o.optionId)}
          >
            <Text size="body" weight={o.kind === 'allow_once' ? 'semibold' : 'regular'} tone={o.kind.startsWith('allow') ? 'text' : 'danger'} className="permission-option-label">{optionLabel(o, prompt.entry.data.edge)}</Text>
            {answering === o.optionId ? <Spinner size={14} /> : null}
          </button>
        ))}
      </div>
    </VStack>
  )
}

/** A bot from outside asking to be let in: the owner's to answer. Neutral, never an agent's colours. */
function BotRequestCard({ bot, outcome, busy, decide }: { bot: BotGuest; outcome: string | null; busy: boolean; decide: (decision: 'allow' | 'deny', always: boolean) => void }) {
  const always = alwaysLabel(bot)
  return (
    <DecisionCard
      kind="permission"
      pair={NEUTRAL_PAIR}
      heading={cardHeading(bot)}
      leading={<BotGuestAvatar provider={bot.provider} size={26} />}
      title={cardQuestion(bot)}
      outcome={outcome ?? undefined}
      pending={busy}
      onAllow={() => decide('allow', false)}
      onDeny={() => decide('deny', false)}
      onAlways={always ? () => decide('allow', true) : undefined}
      alwaysLabel={always ?? undefined}
    />
  )
}
