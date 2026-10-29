import { useState } from 'react'
import { Button } from '../components/Controls'
import { Sheet } from '../components/Overlay'
import { HStack, VStack } from '../components/Stack'
import { Text } from '../components/Text'
import { useModel } from '../lib/observable'
import { prefs, usePref } from '../lib/prefs'
import { afterStarAsk, dueStarAsk } from '../lib/star-ask'
import cube from '../assets/kith-cube.png'
import { useApp } from '../store/context'

const REPO = 'https://github.com/Ying-Kai-Liao/kith'

const copy = {
  first: 'Kith is free and open source. A star on GitHub is how other people find it.',
  second: 'Enjoying Kith? A star on GitHub helps other people find it.',
}

/**
 * Asks for a GitHub star twice at most: once after setup, and once more a week later.
 * Starring ends it.
 */
export function StarPrompt() {
  const app = useApp()
  const local = useModel(app.local)
  const [onboarded] = usePref(prefs.onboardingDone)
  const [state, setState] = usePref(prefs.starAsk)
  const [closed, setClosed] = useState(false)
  const due = dueStarAsk(state, Date.now())
  const open = due !== null && onboarded && !closed && local?.connection.kind === 'online'

  const finish = (starred: boolean) => {
    if (closed) return
    setClosed(true)
    setState(afterStarAsk(state, starred, Date.now()))
    if (starred) window.kith.app.openExternal(REPO)
  }

  const line = copy[due ?? 'first']
  return (
    <Sheet open={open} onClose={() => finish(false)}>
      <VStack align="center" gap={16} className="star-prompt">
        {/* The logo keeps its own colours: never tinted. */}
        <img src={cube} alt="" width={72} height={72} className="star-prompt-cube" />
        <Text as="div" size="callout" className="star-prompt-message">{line}</Text>
        <HStack align="stretch" gap={12}>
          <Button kind="secondary" onClick={() => finish(false)}>
            Not now
          </Button>
          <Button onClick={() => finish(true)}>Star on GitHub</Button>
        </HStack>
      </VStack>
    </Sheet>
  )
}
