import { Icon } from '../Icon'
import { botGuestVariant } from './logic.ts'
import './kith.css'

// The "G" of Kith's Inter SemiBold, drawn in a 26-unit box (not xAI's mark: the default picture is ours).
const G_PATH =
  'M13.207 18.46Q11.737 18.46 10.63 17.8Q9.524 17.14 8.903 15.916Q8.282 14.692 8.282 13.014Q8.282 11.301 8.921 10.077Q9.56 8.853 10.659 8.197Q11.758 7.54 13.15 7.54Q14.32 7.54 15.273 7.99Q16.226 8.439 16.843 9.239Q17.461 10.038 17.625 11.08H15.676Q15.42 10.231 14.781 9.738Q14.142 9.246 13.171 9.246Q12.315 9.246 11.64 9.681Q10.966 10.117 10.58 10.955Q10.195 11.794 10.195 13Q10.195 14.806 11.034 15.78Q11.872 16.754 13.214 16.754Q14.413 16.754 15.127 16.08Q15.841 15.405 15.869 14.256H13.414V12.757H17.718V14.035Q17.718 15.398 17.136 16.394Q16.554 17.389 15.537 17.925Q14.52 18.46 13.207 18.46Z'

/** The default picture of an outside bot: an ink disc with a G for Grok, a quiet bubble disc for any other. Follows light and dark. */
export function BotGuestAvatar({ provider, size = 26 }: { provider: string | null | undefined; size?: number }) {
  const grok = botGuestVariant(provider) === 'grok'
  return (
    <span className={`kc-botguest ${grok ? 'grok' : 'other'}`} style={{ width: size, height: size }} aria-hidden>
      {grok ? (
        <svg viewBox="0 0 26 26" width={size} height={size}>
          <circle cx="13" cy="13" r="13" fill="var(--text)" />
          <path d={G_PATH} fill="var(--background)" />
        </svg>
      ) : (
        <Icon name="bubble.left.and.bubble.right" size={Math.round(size * 0.47)} color="var(--secondary)" />
      )}
    </span>
  )
}
