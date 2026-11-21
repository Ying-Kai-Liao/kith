import { useEffect, useState } from 'react'
import type { InviteLink } from '@shared/invite'
import { mockInvitePreviewProvider, type InvitePreview, type InvitePreviewProvider } from '@shared/invite-preview'
import { Button } from '../components/Controls'
import { Icon } from '../components/Icon'
import { loadPreview } from './invite-preview-logic'

/** Placeholder: the store pages are not decided yet. */
export const GET_APP_URL = 'https://kith.app'

let defaultProvider: InvitePreviewProvider = mockInvitePreviewProvider
/** Swaps the page's provider (the real cloud one later, a fake in tests). */
export const setInvitePreviewProvider = (p: InvitePreviewProvider) => void (defaultProvider = p)

export type PreviewState = { kind: 'loading' } | { kind: 'ready'; preview: InvitePreview } | { kind: 'fallback' }

export function useInvitePreview(link: InviteLink | null | undefined, provider?: InvitePreviewProvider): PreviewState {
  const [state, setState] = useState<PreviewState>({ kind: 'loading' })
  const key = link ? `${link.computerId}/${link.inviteId}` : ''
  useEffect(() => {
    if (!link) return setState({ kind: 'fallback' })
    let live = true
    setState({ kind: 'loading' })
    void loadPreview(provider ?? defaultProvider, link).then((p) => live && setState(p ? { kind: 'ready', preview: p } : { kind: 'fallback' }))
    return () => void (live = false)
    // The link is identified by its ids; the token never changes what is shown.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, provider])
  return state
}

/** What the invite opens: the outcome first, then the choice between the app (primary) and this browser (the backup). */
export function InvitePreviewCard({ link, appLink, onContinue, provider }: { link: InviteLink | null | undefined; appLink?: string; onContinue?: () => void; provider?: InvitePreviewProvider }) {
  const state = useInvitePreview(link, provider)
  return (
    <div className="web-preview" data-screen="invite-preview" data-state={state.kind}>
      {state.kind === 'loading' ? (
        <div className="web-skeleton" aria-busy="true" aria-label="Loading your invite">
          <span />
          <span />
          <span />
        </div>
      ) : state.kind === 'ready' ? (
        <>
          <h1 className="web-title">{state.preview.inviterName} invited you to {state.preview.computerName}</h1>
          <ul className="web-preview-items">
            {state.preview.items.map((item) => (
              <li key={`${item.kind}/${item.id}`} data-kind={item.kind}>
                <Icon name={item.kind === 'agent' ? 'person.crop.circle' : 'person.2'} size={18} color="var(--secondary)" />
                <span className="web-preview-name">
                  {item.name} <span className="web-fine">{item.kind === 'agent' ? 'Agent' : 'Room'}</span>
                </span>
                <span className="web-fine">{item.suitableFor}</span>
              </li>
            ))}
          </ul>
          {state.preview.note ? <p className="web-text">{state.preview.note}</p> : null}
        </>
      ) : (
        <h1 className="web-title">You&apos;ve been invited to a computer on Kith</h1>
      )}
      <div className="web-buttons">
        {appLink ? (
          <a className="web-preview-primary" href={appLink} data-action="open-app">
            Open in Kith app
          </a>
        ) : null}
        {onContinue ? (
          <Button kind="secondary" className="web-wide" onClick={onContinue}>
            Continue in browser
          </Button>
        ) : null}
      </div>
      <p className="web-fine">
        Don&apos;t have it? <a href={GET_APP_URL} data-action="get-app">Get the app</a>
      </p>
    </div>
  )
}
