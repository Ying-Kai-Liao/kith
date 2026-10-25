import { useEffect, useRef, useState } from 'react'
import type { JoinStep } from '@shared/invite-join'
import type { RelayState } from '@shared/relay'
import { Button, Spinner } from '../components/Controls'
import { Icon } from '../components/Icon'
import { Dialog, Sheet } from '../components/Overlay'
import { useModel, useModels } from '../lib/observable'
import { StoreContext, useApp } from '../store/context'
import { account, useAccount, type SignInProvider } from '../store/account'
import { inviteJoin } from '../store/invite-join'
import { refKey } from '../store/app-model'
import { useRelayMacs } from '../store/relay-macs'
import { BotRow } from '../views/BotRow'
import { GrantedAgentsChat } from '../views/computers/GrantedAgentsChat'
import { ThreadView } from '../views/thread/ThreadView'
import type { Unsupported } from './detect'
import { emptyAccountText, noticeFor } from './notice'
import { webSession } from './session'
import './web.css'

/**
 * The web client's whole UI: a member's view of the computers shared with them (design §2, §4.7). Join, sign in, the chats
 * of the agents they were given, a thread and a composer, sign out. It reuses the renderer's thread, composer and rows;
 * settings, roster management, marketplace, people and the terminal are not reachable from here. Hiding them is not the
 * security boundary (the host is), it is less code to audit.
 */
export function WebWindow() {
  const [support, setSupport] = useState<Unsupported | null | undefined>(undefined)
  useEffect(() => void webSession.support().then(setSupport), [])
  if (support === undefined) return <Splash />
  if (support) return <TooOld reason={support} />
  return <Gate />
}

function Splash() {
  return (
    <main className="web-center" data-screen="loading">
      <Spinner size={22} />
    </main>
  )
}

const REASONS: Record<Unsupported, string> = {
  webcrypto: 'secure key storage is missing (this page needs https)',
  ed25519: 'Ed25519 keys are not supported',
  indexeddb: 'IndexedDB is not available (a private window can cause this)',
  websocket: 'WebSocket is not available',
}

/** No fallback exists: an extractable key would defeat the design (§4.2), so an old browser is told to update. */
export function TooOld({ reason }: { reason: Unsupported }) {
  return (
    <main className="web-center" data-screen="too-old">
      <div className="web-card">
        <Icon name="exclamationmark.triangle" size={28} color="var(--secondary)" />
        <h1 className="web-title">This browser is too old for Kith</h1>
        <p className="web-text">
          Kith keeps a private key in your browser that nothing, not even Kith, can copy out. This browser can&apos;t do that yet.
          Update it, or use a current Chrome, Safari or Firefox, then open your invite link again.
        </p>
        <p className="web-fine">Missing: {REASONS[reason]}</p>
      </div>
    </main>
  )
}

function Gate() {
  const acct = useAccount()
  const [loaded, setLoaded] = useState(false)
  useEffect(() => void webSession.ready().then(() => window.kith.account.state()).then(() => setLoaded(true)), [])
  if (!loaded || !acct.isReady) return <Splash />
  return acct.isSignedIn ? <MemberApp /> : <SignIn />
}

// ---- signed out ----

function SignIn() {
  const acct = useAccount()
  const [invited, setInvited] = useState(false)
  useEffect(() => void webSession.pendingLink().then((l) => setInvited(!!l)), [])
  const providers: { id: SignInProvider; label: string }[] = [
    { id: 'google', label: 'Continue with Google' },
    { id: 'github', label: 'Continue with GitHub' },
  ]
  return (
    <main className="web-center" data-screen="sign-in">
      <div className="web-card">
        <h1 className="web-title">{invited ? "You've been invited to Kith" : 'Kith'}</h1>
        <p className="web-text">
          {invited
            ? "Sign in to join. Your invite is kept safe in this browser while you do."
            : 'Sign in to use the agents on computers that were shared with you. To join one, open the invite link you were sent.'}
        </p>
        <div className="web-buttons">
          {providers.map((p) => (
            <Button key={p.id} kind="secondary" className="web-wide" onClick={() => void account.signIn(p.id)} disabled={acct.isBusy}>
              {p.label}
            </Button>
          ))}
        </div>
        {acct.errorMessage ? <p className="web-error" role="alert">{acct.errorMessage}</p> : null}
        <p className="web-fine">Signing in does not give access to anything by itself. The computer&apos;s owner decides what you can use.</p>
      </div>
    </main>
  )
}

// ---- signed in: join, list, thread ----

const progress: Record<JoinStep, string> = {
  signingIn: 'Signing in…',
  joining: 'Joining…',
  asking: 'Asking the computer for access…',
  connecting: 'Connecting…',
  proving: 'Checking your invite…',
  done: "You're in",
  waiting: 'Waiting for the computer',
  failed: "Couldn't join",
}

function JoinPanel() {
  const join = useModel(inviteJoin)
  const state = join.state
  const busy = state !== null && ['signingIn', 'joining', 'asking', 'connecting', 'proving'].includes(state.step)
  const close = () => {
    void webSession.discardPending()
    inviteJoin.dismiss()
  }
  return (
    <div className="web-center" data-screen="join" data-step={state?.step ?? 'starting'}>
      <div className="web-card">
        <div className="web-row-center">
          {busy || !state ? <Spinner size={18} /> : <Icon name={state.step === 'done' ? 'checkmark.circle' : state.step === 'failed' ? 'exclamationmark.triangle' : 'person.crop.circle'} size={22} color="var(--secondary)" />}
          <span className="web-title-small">{state ? progress[state.step] : 'Getting your invite…'}</span>
        </div>
        {state?.message ? <p className={state.step === 'failed' ? 'web-error' : 'web-text'} role={state.step === 'failed' ? 'alert' : undefined}>{state.message}</p> : null}
        {state?.step === 'waiting' ? <Button kind="secondary" onClick={() => void inviteJoin.run()}>Try again</Button> : null}
        {state?.step === 'done' || state?.step === 'waiting' || state?.step === 'failed' ? <Button onClick={close}>{state.step === 'done' ? 'Open chats' : 'Close'}</Button> : null}
      </div>
    </div>
  )
}

function ConfirmJoin({ computerId, appLink, onJoin, onDecline }: { computerId: string; appLink?: string; onJoin: () => void; onDecline: () => void }) {
  return (
    <div className="web-center" data-screen="confirm-join">
      <div className="web-card">
        <h1 className="web-title">Join a computer on Kith?</h1>
        <p className="web-text">
          An invite link asked this browser to join the computer with ID {computerId.slice(0, 8)}. Only continue if you expect this invite
          from someone you know. Its owner will see that you joined and decides which agents you can use.
        </p>
        <div className="web-buttons">
          <Button className="web-wide" onClick={onJoin}>Join</Button>
          <Button kind="secondary" className="web-wide" onClick={onDecline}>Not now</Button>
        </div>
        {appLink ? (
          <p className="web-fine">
            Have the Kith app? <a href={appLink} data-action="open-app">Open in the Kith app</a>
          </p>
        ) : null}
      </div>
    </div>
  )
}

function MemberApp() {
  const app = useApp()
  const macs = useRelayMacs(app)
  const join = useModel(inviteJoin)
  useModels([...app.stores.values()])
  const acct = useAccount()
  const [states, setStates] = useState<Record<string, RelayState>>({})
  const [newChat, setNewChat] = useState<string | null>(null)
  const [menu, setMenu] = useState(false)
  const [confirmOut, setConfirmOut] = useState(false)

  useEffect(() => window.kith.relay.onState((id, s) => setStates((prev) => ({ ...prev, [id]: s }))), [])

  // An invite waiting in IndexedDB (or one that survived a reload mid-join) is offered, never run by itself: a link an
  // agent posted and the person opened while signed in must not join anything without a click.
  const [offer, setOffer] = useState<{ computerId: string; appLink?: string } | null | undefined>(undefined)
  useEffect(() => void webSession.pendingLink().then(setOffer), [])
  useEffect(() => {
    inviteJoin.adopt = (id) => app.relayMacs.adopt(id)
    return () => void (inviteJoin.adopt = null)
  }, [app])

  // A returning visit goes straight to connect: every computer shared with this account is attached, once each.
  const tried = useRef(new Set<string>())
  useEffect(() => {
    // Not while a join is running: it asks for access itself, and a second request would cancel the first.
    if (join.link !== null || offer !== null) return
    for (const c of macs.computers) {
      if (c.role !== 'member' || tried.current.has(c.computerId) || macs.store(c.computerId) || macs.access.has(c.computerId)) continue
      tried.current.add(c.computerId)
      void macs.connect(c.computerId)
    }
  })

  const shared = macs.computers.filter((c) => c.role === 'member')
  const selected = app.selection
  const selectedStore = app.selectedStore
  const showThread = !!(selected && selectedStore && selectedStore.bots.get(selected.botId))

  if (join.link !== null) return <Shell>{<JoinPanel />}</Shell>
  if (offer) {
    return (
      <Shell>
        <ConfirmJoin
          computerId={offer.computerId}
          appLink={offer.appLink}
          onJoin={() => {
            setOffer(null)
            void inviteJoin.start()
          }}
          onDecline={() => {
            setOffer(null)
            void webSession.discardPending()
          }}
        />
      </Shell>
    )
  }

  return (
    <Shell
      header={
        <header className="web-top">
          {showThread ? (
            <button className="web-back" onClick={() => app.select(null)} aria-label="Back to chats">
              <Icon name="chevron.left" size={16} weight="semibold" />
              <span>Chats</span>
            </button>
          ) : (
            <span className="web-brand">Kith</span>
          )}
          <button className="web-account" onClick={() => setMenu((m) => !m)} aria-expanded={menu} aria-label="Account">
            <Icon name="person.crop.circle" size={20} />
          </button>
          {menu ? (
            <div className="web-menu" role="menu">
              <p className="web-fine">{acct.user?.email ?? 'Signed in'}</p>
              <Button kind="secondary" onClick={() => { setMenu(false); setConfirmOut(true) }}>Sign out</Button>
            </div>
          ) : null}
        </header>
      }
    >
      <div className="web-main" data-view={showThread ? 'thread' : 'list'}>
        <nav className="web-list" aria-label="Chats">
          {!macs.loaded ? <div className="web-pad"><Spinner size={16} /></div> : null}
          {macs.loaded && shared.length === 0 ? (
            <div className="web-pad">
              <p className="web-text" data-screen="empty-account">{macs.error ?? emptyAccountText(macs.computers)}</p>
              <Button kind="secondary" onClick={() => setConfirmOut(true)}>Sign out</Button>
            </div>
          ) : null}
          {shared.map((c) => {
            const store = app.store(c.computerId)
            const notice = noticeFor(states[c.computerId] ?? null, c.name)
            const access = macs.access.get(c.computerId)
            return (
              <section key={c.computerId} className="web-computer" data-computer={c.computerId}>
                <div className="web-computer-head">
                  <span className="web-computer-name">{c.name}</span>
                  {store?.shownConnection.kind === 'online' ? (
                    <button className="web-new" onClick={() => setNewChat(c.computerId)}>
                      <Icon name="square.and.pencil" size={14} weight="medium" />
                      <span>New chat</span>
                    </button>
                  ) : null}
                </div>
                {notice.kind !== 'none' ? (
                  <div className={`web-notice ${notice.kind}`} role={notice.kind === 'error' || notice.kind === 'removed' ? 'alert' : 'status'}>
                    <span>{notice.text}</span>
                    {notice.kind === 'signIn' ? <Button kind="secondary" onClick={() => void webSession.signInAgain()}>Sign in again</Button> : null}
                    {notice.kind === 'removed' ? <Button kind="secondary" onClick={() => location.reload()}>Reload</Button> : null}
                  </div>
                ) : null}
                {access?.kind === 'failed' ? <div className="web-notice error" role="alert"><span>{access.message}</span><Button kind="secondary" onClick={() => { tried.current.delete(c.computerId); macs.access.delete(c.computerId); void macs.connect(c.computerId) }}>Try again</Button></div> : null}
                {access?.kind === 'asking' || access?.kind === 'code' ?<div className="web-notice info"><Spinner size={12} /><span>Asking {c.name} for access…</span></div> : null}
                {store ? (
                  store.roster.length === 0 ? (
                    <p className="web-fine web-pad">{store.shownConnection.kind === 'online' ? 'No chats yet. Start one with New chat.' : 'Chats appear once the computer is reachable.'}</p>
                  ) : (
                    store.roster.map((bot) => (
                      <button
                        key={bot.id}
                        className={`web-chat ${selected && refKey(selected) === `${c.computerId}/${bot.id}` ? 'selected' : ''}`}
                        onClick={() => app.select({ computerId: c.computerId, botId: bot.id })}
                      >
                        <BotRow bot={bot} store={store} compact={false} usingComputer={false} members={[]} waiting={0} />
                      </button>
                    ))
                  )
                ) : null}
              </section>
            )
          })}
        </nav>
        <section className="web-thread" aria-label="Conversation">
          {showThread && selected && selectedStore ? (
            <StoreContext.Provider value={selectedStore}>
              <ThreadView key={refKey(selected)} botId={selected.botId} />
            </StoreContext.Provider>
          ) : (
            <div className="web-center web-empty">
              <p className="web-text">Pick a chat to read it.</p>
            </div>
          )}
        </section>
      </div>

      <Sheet open={newChat !== null} onClose={() => setNewChat(null)} width={360}>
        {newChat && app.store(newChat) ? (
          <StoreContext.Provider value={app.store(newChat)!}>
            <GrantedAgentsChat onClose={() => setNewChat(null)} />
          </StoreContext.Provider>
        ) : null}
      </Sheet>
      <Dialog
        open={confirmOut}
        title="Sign out of Kith?"
        message="This removes this browser's key and your chats from it. To come back, open your invite link again."
        actions={[{ title: 'Sign out', destructive: true, action: () => void account.signOut() }]}
        onClose={() => setConfirmOut(false)}
      />
      <Dialog open={app.errorMessage !== null} title="Something went wrong" message={app.errorMessage} actions={[]} cancel="OK" onClose={() => app.clearErrors()} />
    </Shell>
  )
}

function Shell({ header, children }: { header?: React.ReactNode; children: React.ReactNode }) {
  return (
    <div className="web-shell">
      {header}
      {children}
    </div>
  )
}
