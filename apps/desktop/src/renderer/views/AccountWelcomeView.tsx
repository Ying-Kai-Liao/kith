import { useEffect, useState, type CSSProperties } from 'react'
import { LiveBot, type Mood } from '../components/Avatar'
import { Button, Spinner } from '../components/Controls'
import { GitHubMark } from '../components/GitHubMark'
import { Icon } from '../components/Icon'
import { HStack, Spacer, VStack } from '../components/Stack'
import { Text } from '../components/Text'
import { useReduceMotion } from '../lib/theme'
import { useAccount } from '../store/account'
import googleLogo from '../assets/google.svg'
import './account-welcome.css'

const deviceName = window.kith.platform === 'darwin' ? 'Mac' : 'computer'

/** First-run account choice. A restored session skips this screen as soon as it is back. */
export function AccountWelcomeView({ onContinue }: { onContinue: () => void }) {
  const account = useAccount()
  const reduce = useReduceMotion()
  const [beat, setBeat] = useState(reduce ? 3 : 0)

  useEffect(() => {
    if (account.isSignedIn) onContinue()
  }, [account.isSignedIn, onContinue])

  useEffect(() => {
    if (reduce) return setBeat(3)
    const timers = [150, 430, 710].map((at, i) => setTimeout(() => setBeat(i + 1), at))
    return () => timers.forEach(clearTimeout)
  }, [reduce])

  if (!account.isReady) {
    // Keep first-run sign-in choices behind the persisted-session restore on launch.
    return (
      <VStack align="center" justify="center" gap={14} className="welcome-restoring">
        <Spinner size={24} />
        <Text as="div" size="callout" tone="secondary">Checking for a saved account…</Text>
        <button className="welcome-skip" onClick={onContinue}>
          Continue on this {deviceName}
        </button>
      </VStack>
    )
  }

  const rise = (shown: boolean): CSSProperties => ({
    opacity: shown ? 1 : 0,
    transform: shown ? 'none' : 'translateY(14px)',
    transition: 'opacity 0.6s var(--ease-out), transform 0.6s var(--ease-out)',
  })

  return (
    <div className="welcome">
      <div className="kith-sky-home" aria-hidden />
      <VStack className="welcome-column">
        <Spacer min={24} />
        <WelcomeCrew shown={beat >= 1} />
        <Spacer min={24} />
        <VStack gap={10} className="welcome-text" style={rise(beat >= 2)}>
          <div className="welcome-title">Your coding agents, as teammates.</div>
          <Text as="div" size={14} tone="secondary">Give each one a name and a project. They work on your computer while you're away.</Text>
        </VStack>
        <Spacer min={32} />
        <VStack gap={8} style={rise(beat >= 3)}>
          <Button onClick={onContinue} className="welcome-button welcome-primary">
            Continue on this {deviceName}
          </Button>
          {account.appleSignIn ? (
            <Button kind="secondary" onClick={() => void account.signIn('apple')} disabled={!account.isConfigured || account.isBusy} className="welcome-button">
              {account.isBusy ? (
                <Spinner size={18} />
              ) : (
                <HStack as="span" gap={10}>
                  <Icon name="apple.logo" size={20} />
                  Continue with Apple
                </HStack>
              )}
            </Button>
          ) : null}
          <Button kind="secondary" onClick={() => void account.signIn('google')} disabled={!account.isConfigured || account.isBusy} className="welcome-button">
            {account.isBusy ? (
              <Spinner size={18} />
            ) : (
              <HStack as="span" gap={10}>
                <img src={googleLogo} alt="" width={20} height={20} />
                Continue with Google
              </HStack>
            )}
          </Button>
          <Button kind="secondary" onClick={() => void account.signIn('github')} disabled={!account.isConfigured || account.isBusy} className="welcome-button">
            {account.isBusy ? (
              <Spinner size={18} />
            ) : (
              <HStack as="span" gap={10}>
                <GitHubMark size={20} />
                Continue with GitHub
              </HStack>
            )}
          </Button>
          {!account.isConfigured ? (
            <Text as="div" size="footnote" tone="tertiary" className="welcome-footnote">Sign-in isn't configured in this build. You can still use Kith on this {deviceName}.</Text>
          ) : null}
          {account.errorMessage ? <Text as="div" size="footnote" tone="danger" className="welcome-footnote">{account.errorMessage}</Text> : null}
        </VStack>
      </VStack>
    </div>
  )
}

const crew: { name: string; size: number; x: number; y: number; mood: Mood }[] = [
  { name: 'builder', size: 92, x: 0, y: 0, mood: 'working' },
  { name: 'designer', size: 64, x: -104, y: -34, mood: 'idle' },
  { name: 'coder', size: 58, x: 100, y: -46, mood: 'working' },
  { name: 'lab', size: 48, x: -76, y: 64, mood: 'idle' },
  { name: 'house', size: 52, x: 84, y: 58, mood: 'needsInput' },
]

/** The animated bot group from the first iPhone onboarding screen. */
function WelcomeCrew({ shown }: { shown: boolean }) {
  const reduce = useReduceMotion()
  const [t, setT] = useState(0)
  useEffect(() => {
    if (reduce || !shown) return
    let frame = 0
    const tick = (now: number) => {
      setT(now / 1000)
      frame = requestAnimationFrame(tick)
    }
    frame = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(frame)
  }, [reduce, shown])
  return (
    <div aria-hidden className="welcome-crew">
      {crew.map((m, i) => (
        <div
          key={m.name}
          className="welcome-crew-member"
          style={{
            transform: `translate(calc(-50% + ${m.x}px), calc(-50% + ${m.y + (reduce ? 0 : Math.sin(t * 1.3 + i * 1.7) * 5)}px)) scale(${shown ? 1 : 0.3})`,
            opacity: shown ? 1 : 0,
            transition: `opacity 0.7s var(--ease-out) ${i * 0.07}s, scale 0.7s var(--ease-out) ${i * 0.07}s`,
          }}
        >
          <LiveBot face={{ kind: 'crew', name: m.name }} size={m.size} id={m.name} mood={m.mood} />
        </div>
      ))}
    </div>
  )
}
