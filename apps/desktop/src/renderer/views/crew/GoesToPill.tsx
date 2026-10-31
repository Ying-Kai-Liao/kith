import { useRef, useState } from 'react'
import type { GoesTo } from '@shared/goes-to'
import { Icon } from '../../components/Icon'
import { AnchoredMenu, type MenuItem } from '../../components/Overlay'

/**
 * "Goes to: coder · new session", over the home box while text is typed. Tapping it offers the two next most recent
 * agents and then everyone. An @name picks the agent on its own, so there is nothing to choose; an unknown @name shows
 * no pill at all.
 */
export function GoesToPill({ route, onPick }: { route: GoesTo; onPick: (key: string) => void }) {
  const [open, setOpen] = useState(false)
  const anchor = useRef<HTMLButtonElement>(null)
  if (!route.target) return null
  const items = (): MenuItem[] => [
    ...route.alternatives.map((a) => ({ title: a.name, action: () => onPick(a.key) })),
    ...route.all.map((a, i) => ({ title: a.name, selected: a.key === route.target?.key, divider: i === 0 && route.alternatives.length > 0, action: () => onPick(a.key) })),
  ]
  const pickable = route.how === 'auto' && route.all.length > 1
  return (
    <div className="cs-goes no-drag">
      <button ref={anchor} className="cs-goes-pill kith-glass" disabled={!pickable} aria-haspopup={pickable ? 'menu' : undefined} onClick={() => setOpen(true)}>
        Goes to: {route.target.name} · new session
        {pickable ? <Icon name="chevron.down" size={9} weight="bold" /> : null}
      </button>
      <AnchoredMenu open={open} onClose={() => setOpen(false)} anchor={anchor} items={items} />
    </div>
  )
}
