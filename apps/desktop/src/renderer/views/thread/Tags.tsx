import { Icon } from '../../components/Icon'
import './conversation.css'

/** The capsule after a bot's name, on every line it writes and on its member row (DESIGN.md "Words": bot, not agent). */
export function BotTag() {
  return <span className="bot-tag">Bot</span>
}

/** On a reply that came after its run was over. */
export function LateTag() {
  return (
    <span className="late-tag">
      <Icon name="clock.arrow.circlepath" size={10} weight="medium" />
      Late
    </span>
  )
}
