import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type RefObject } from 'react'
import { isGroup, needsInput, type Bot } from '@shared/models'
import { avatarImage, type AvatarImage } from '@shared/crew'
import {
  BLINK_MS,
  BODY_IDLE_MS,
  BODY_NEEDS_LOOP_MS,
  BODY_WORKING_MS,
  closedScale,
  doneDue,
  GROUP_TRANSFORM,
  loopsOf,
  LOOK_ERROR_MS,
  GLANCE_MS,
  glanceSide,
  LOOK_IDLE_MS,
  LOOK_WORKING_MS,
  NEEDS_ENTER_GAP_MS,
  paletteColor,
  phaseMs,
  rigOf,
  sizeTier,
  type Tier,
} from '@shared/living-bots'
import { useReduceMotion } from '../lib/theme'
import { Icon } from './Icon'
import { bodyURL, cachedRecolour, recolour } from './living-bots-art'
import { scheduleSettle, watch, type Slot } from './living-bots-window'
import './living-bots.keyframes.css'
import './living-bots.css'
import './avatar.css'

/** What a living bot is doing: the bot's status, which MOTION.md gives a body motion and a gaze each. */
export type Mood = 'idle' | 'working' | 'needsInput' | 'error'

/** A crew member or spare, or the kith cube on the group tile. */
export type Face = AvatarImage | 'group'

/** The mount phase of every loop, as the negative animation delays MOTION.md section 3 asks for. */
function phaseVars(id: string, now = Date.now()) {
  const f = loopsOf(id)
  const delay = (length: number, fraction: number) => `-${phaseMs(length, fraction, now).toFixed(0)}ms`
  return {
    '--p-look': delay(LOOK_IDLE_MS[f.lookTrack]!, f.fLook),
    '--p-blink': delay(BLINK_MS[f.blinkTrack]!, f.fBlink),
    '--p-glance': delay(GLANCE_MS, f.fLook),
    '--p-work': delay(LOOK_WORKING_MS, f.fLook),
    '--p-err': delay(LOOK_ERROR_MS, f.fLook),
    '--p-idle': delay(BODY_IDLE_MS, f.fBody),
    '--p-bwork': delay(BODY_WORKING_MS, f.fBody),
    '--p-needs': delay(BODY_NEEDS_LOOP_MS, f.fBody),
  }
}

type Shown = { mood: Mood; flag: '' | 'done' | 'enter' }

/** When each bot last played the needs-you double hop: a bot that flaps does not hop twice within the gap. */
const lastEnter = new Map<string, number>()

/**
 * Reads the presented body and gaze, then eases both to rest (MOTION.md "Handoff") before the new state
 * starts. CSS animations are held off by `data-settling` meanwhile; the blink loop is never touched.
 */
function handoff(el: HTMLElement) {
  const body = el.querySelector<HTMLElement>('.kb-body')
  const gaze = el.querySelector<HTMLElement>('.kb-gaze')
  if (!body || !gaze) return
  const from = (value: string, rest: string) => (value === 'none' || !value ? rest : value)
  const b = getComputedStyle(body)
  const g = getComputedStyle(gaze)
  const bodyFrom = { translate: from(b.translate, '0px 0px'), scale: from(b.scale, '1 1') }
  const gazeFrom = { translate: from(g.translate, '0px 0px') }
  el.dataset.settling = ''
  body.animate([bodyFrom, { translate: '0px 0px', scale: '1 1' }], { duration: 160, easing: 'cubic-bezier(0.37, 0, 0.63, 1)' })
  gaze.animate([gazeFrom, { translate: '0px 0px' }], { duration: 110, easing: 'cubic-bezier(0.3, 0, 0.15, 1)' })
}

/**
 * The state the avatar shows. A change plays the handoff first, then enters the new state: with the done
 * one-shot after >= 1 s of work (at most once per 4 s), and the needs-you double hop (at most once per 10 s).
 */
function useShown(ref: RefObject<HTMLDivElement | null>, mood: Mood, animate: boolean, id: string) {
  const [shown, setShown] = useState<Shown>({ mood, flag: '' })
  const track = useRef({ mood, workingSince: Date.now(), lastDone: -Infinity })
  const mounted = useRef(false)

  useEffect(() => {
    const t = track.current
    if (mood === t.mood) return
    const now = Date.now()
    const before = t.mood
    const worked = now - t.workingSince
    t.mood = mood
    if (mood === 'working') t.workingSince = now
    const el = ref.current
    if (!animate || !el) {
      setShown({ mood, flag: '' })
      return
    }
    handoff(el)
    let clear: ReturnType<typeof setTimeout> | undefined
    const timer = setTimeout(() => {
      let flag: Shown['flag'] = ''
      if (before === 'working' && mood === 'idle' && doneDue(worked, now - t.lastDone)) {
        flag = 'done'
        t.lastDone = Date.now()
      } else if (mood === 'needsInput' && Date.now() - (lastEnter.get(id) ?? -Infinity) >= NEEDS_ENTER_GAP_MS) {
        flag = 'enter'
        lastEnter.set(id, Date.now())
      }
      setShown({ mood, flag })
      // The one-shot is over (900 / 1500 ms): drop the flag so a later freeze and thaw does not replay it.
      if (flag) clear = setTimeout(() => setShown((s) => (s.flag ? { mood: s.mood, flag: '' } : s)), flag === 'done' ? 1000 : 1600)
    }, 160)
    return () => {
      clearTimeout(timer)
      clearTimeout(clear)
    }
  }, [mood, animate, id, ref])

  // The new state is in the DOM: end the handoff, and start its loops at phase 0 (a runtime change, not a mount).
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    delete el.dataset.settling
    if (mounted.current) el.dataset.live = ''
    mounted.current = true
  }, [shown, ref])

  return shown
}

/**
 * One living bot: the layered body with two code-drawn eyes (eyes.json), moved by CSS keyframes only.
 * Size picks how much of it runs (MOTION.md section 7); the window's observer freezes it off screen.
 */
export const LiveBot = memo(function LiveBot({
  face,
  color = '',
  mood = 'idle',
  size,
  id,
  animated = true,
}: {
  face: Face
  /** An avatarColor (palette id); anything else keeps the original body. */
  color?: string
  mood?: Mood
  size: number
  /** The bot's id: it picks the bot's loops and phase, the same on every screen and on iOS. */
  id: string
  animated?: boolean
}) {
  const group = face === 'group'
  const name = group ? 'kith' : face.name
  const rig = rigOf(name)
  const reduce = useReduceMotion()
  const tier: Tier = animated ? sizeTier(size) : 'static'
  const ref = useRef<HTMLDivElement>(null)
  const shown = useShown(ref, mood, tier !== 'static' && !reduce, id)
  const phases = useMemo(() => phaseVars(id), [id])
  const loops = loopsOf(id)

  // Recoloured bodies are rendered once per (bot, colour); until then the original body shows.
  const tinted = !group && rig?.tint && paletteColor(color) ? color : ''
  const [, rendered] = useState(0)
  useEffect(() => {
    if (!tinted || cachedRecolour(name, tinted)) return
    let live = true
    void recolour(name, tinted).then(() => live && rendered((n) => n + 1))
    return () => {
      live = false
    }
  }, [name, tinted])

  // Put the avatar under the window's watch (freeze off screen, cap on full ones, pause when hidden).
  const slot = useRef<Slot | null>(null)
  useEffect(() => {
    const el = ref.current
    if (!el) return
    const entry: Slot = {
      el,
      tier,
      visible: true,
      rephase: (all) => {
        for (const [key, value] of Object.entries(phaseVars(id))) if (all || key !== '--p-blink') el.style.setProperty(key, value)
        delete el.dataset.live
      },
    }
    slot.current = entry
    return watch(entry)
    // The slot lives as long as the element; its tier is kept up to date below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id])
  useEffect(() => {
    if (!slot.current) return
    slot.current.tier = tier
    scheduleSettle()
  }, [tier])

  if (!rig) return <span className="kb" style={{ width: size, height: size }} />
  const src = (tinted && cachedRecolour(name, tinted)) || bodyURL(name) || undefined
  const style = {
    '--kb-size': `${size}px`,
    '--kb-mx': `${Math.min(rig.look.left, rig.look.right) * size}px`,
    '--kb-my': `${Math.min(rig.look.up, rig.look.down) * size}px`,
    '--kb-closed': closedScale(rig.eyes[0].h, size),
    '--kb-glance-side': glanceSide(id),
    '--kb-ox': `${rig.anchor.x * 100}%`,
    '--kb-oy': `${rig.anchor.y * 100}%`,
    '--kb-look-name': `kb-look-idle-${loops.lookTrack}`,
    '--kb-look-dur': `${LOOK_IDLE_MS[loops.lookTrack]}ms`,
    '--kb-blink-name': `kb-blink-${loops.blinkTrack}`,
    '--kb-blink-dur': `${BLINK_MS[loops.blinkTrack]}ms`,
    ...phases,
  } as CSSProperties
  const rigStyle: CSSProperties | undefined = group
    ? { transform: `translate(${GROUP_TRANSFORM.dx * size}px, ${GROUP_TRANSFORM.dy * size}px) scale(${GROUP_TRANSFORM.scale})` }
    : undefined
  return (
    <div
      ref={ref}
      aria-hidden
      className="kb"
      data-eff={tier}
      data-state={shown.mood}
      data-flag={shown.flag || undefined}
      data-reduce={reduce || undefined}
      style={style}
    >
      <div className="kb-rig" style={rigStyle}>
        <div className="kb-body">
          <img className="kb-img" src={src} alt="" draggable={false} />
          <div className="kb-gaze">
            {rig.eyes.map((eye, i) => (
              <div
                key={i}
                className="kb-eye"
                style={{ left: (eye.x - eye.w / 2) * size, top: (eye.y - eye.h / 2) * size, width: eye.w * size, height: eye.h * size, rotate: `${eye.tilt}deg` }}
              >
                <div className="kb-shape" />
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  )
})

/** A bot's mood from its status. A bot shown `animated={false}` (a chip) is always at rest. */
export function moodOf(bot: Bot, animated = true): Mood {
  if (!animated) return 'idle'
  return bot.status === 'error' ? 'error' : needsInput(bot) ? 'needsInput' : bot.status === 'working' ? 'working' : 'idle'
}

/** A crew or spare portrait at rest (the editor's preview and picker). */
export function CrewPortrait({ image, size = 40, color = '' }: { image: AvatarImage; size?: number; color?: string }) {
  return <LiveBot face={image} color={color} size={size} id={image.name} animated={false} />
}

export function BotAvatar({ bot, size = 40, animated = true }: { bot: Bot; size?: number; animated?: boolean }) {
  const face = isGroup(bot) ? 'group' : (avatarImage(bot) ?? 'group')
  return <LiveBot face={face} color={bot.avatarColor} mood={moodOf(bot, animated)} size={size} id={bot.id} animated={animated} />
}

/** A group chat's face: the kith cube, no tile. */
export function GroupAvatar({ size = 40, animated = true }: { members?: Bot[]; size?: number; animated?: boolean }) {
  return (
    <div aria-hidden className="avatar-frame" style={{ width: size, height: size }}>
      <LiveBot face="group" size={size} id="kith" animated={animated} />
    </div>
  )
}

/** Avatar with the roster status dot: filled = unread, amber = needs you. A group shows the cube. */
export function AvatarWithStatus({ bot, size = 44 }: { bot: Bot; members?: Bot[]; size?: number }) {
  return (
    <div className="avatar-frame" style={{ width: size, height: size }}>
      <BotAvatar bot={bot} size={size} />
      {needsInput(bot) ? (
        <div className="avatar-badge needs-you" style={{ width: size * 0.36, height: size * 0.36 }}>
          <Icon name="exclamationmark" size={size * 0.2} weight="black" scaled={false} />
        </div>
      ) : bot.unread > 0 ? (
        <div className="avatar-badge unread" style={{ width: size * 0.28, height: size * 0.28 }} />
      ) : null}
    </div>
  )
}
