import { alwaysLabel, cardHeading, cardQuestion, type BotGuest } from '@shared/bot-guests'
import { promptWho, type GuestPrompt } from '@shared/mac-role'
import { isSecretCard } from '@shared/secret-card'
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
import { AccessCard, KeyCard } from './thread/SecretCards'
import { optionLabel, sortedOptions } from './thread/PermissionCard'
import { agentBot } from './usage/SpendSection'

/**
 * "Needs your OK": what is asked outside this account's chats, for it to answer: other people's agents on this Mac, and
 * the cards of an agent with no chat here (its card lane), key and account cards among them. The data and the answering are
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
      {prompts.map((p) => isSecretCard(p.entry) ? (
        <SecretPrompt key={p.entryId} prompt={p} store={store} />
      ) : limitOf(p.entry) ? (
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
      <Text size="callout" weight="medium" truncate>{promptWho(prompt)}</Text>
      <Text size="caption" tone="secondary" mono lines={3}>{prompt.title}</Text>
      <div className="permission-card-options">
        {sortedOptions(prompt.options).map((o) => (
          <button
            key={o.optionId}
            className={answering === null || answering === o.optionId ? 'permission-option' : 'permission-option dimmed'}
            disabled={answering !== null}
            onClick={() => respond(o.optionId)}
          >
            <Text size="body" weight={o.kind === 'allow_once' ? 'semibold' : 'regular'} tone={o.kind.startsWith('allow') ? 'text' : 'danger'} className="permission-option-label">{optionLabel(o)}</Text>
            {answering === o.optionId ? <Spinner size={14} /> : null}
          </button>
        ))}
      </div>
    </VStack>
  )
}

/** A key or account card, drawn as in a chat. */
function SecretPrompt({ prompt, store }: { prompt: GuestPrompt; store: BotStore }) {
  const bot = agentBot(store, prompt.agent)
  const common = { entry: prompt.entry, agent: bot?.name ?? (prompt.agent || 'The agent'), pair: pairOf(bot?.avatarColor), answering: store.answering.get(prompt.entryId) ?? null, respond: (o: string) => store.respond(prompt.entry, o) }
  return prompt.entry.data.card === 'account' ? <AccessCard {...common} /> : <KeyCard {...common} save={(account, values) => store.provideSecret(prompt.entry, account, values).then(() => undefined)} />
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
