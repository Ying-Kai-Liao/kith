import type { InviteLink } from '@shared/invite'
import { runJoin, type JoinDeps, type JoinState } from '@shared/invite-join'
import { scopedKey } from '@shared/mac-role'
import { relayJoinDeps } from '@shared/relay-access'
import { Observable } from '../lib/observable'
import { account } from './account'

/** The real dependencies of a join; `runJoin` takes them so the order can be tested with fakes. */
export function appJoinDeps(waitForSignIn: () => Promise<void>, cancelled: () => boolean = () => false): JoinDeps {
  return relayJoinDeps(
    window.kith.relay,
    {
      signedIn: () => account.isSignedIn,
      waitForSignIn,
      async accept(inviteId, h1) {
        const res = await window.kith.cloud.acceptInvite(inviteId, h1)
        if (!res.ok) throw res.error
        return res.value as { computerId: string; membershipId?: string }
      },
      keepToken: (computerId, inviteId, token) => window.kith.invites.keep(scopedKey(account.user?.userId ?? '', computerId), inviteId, token),
    },
    // The six digits reach the join's state through runJoin's onCode: in Code mode the owner compares them.
    { cancelled },
  )
}

/** The invite link being joined, and how far it got. */
export class InviteJoin extends Observable {
  link: InviteLink | null = null
  state: JoinState | null = null
  private cancelSignIn: ((e: Error) => void) | null = null
  private running = false
  private closed = false
  /** Set by the window: makes the joined Mac one this device keeps connected. */
  adopt: ((computerId: string) => Promise<void>) | null = null

  /** A link arrived (or one was waiting at launch): runs the join. */
  async start() {
    if (this.running) return
    const link = await window.kith.invites.takePending()
    if (!link) return
    this.link = link
    await this.run()
  }

  async run() {
    const link = this.link
    if (!link || this.running) return
    this.running = true
    this.closed = false
    try {
      const end = await runJoin(
        link,
        appJoinDeps(
          () =>
            new Promise<void>((resolve, reject) => {
              this.cancelSignIn = reject
              const stop = account.subscribe(() => {
                if (account.isSignedIn) {
                  stop()
                  this.cancelSignIn = null
                  resolve()
                }
              })
            }),
          () => this.closed,
        ),
        (s) => {
          this.state = s
          this.changed()
        },
      )
      // Done, or the Mac is only offline for now: either way the token is kept and the Mac belongs on this device.
      if ((end.step === 'done' || end.step === 'waiting') && end.computerId) await this.adopt?.(end.computerId)
    } finally {
      this.running = false
    }
  }

  /** The person closed the sheet. The link is dropped; the token stays kept only if the cloud already took the invite. */
  dismiss() {
    this.closed = true
    this.cancelSignIn?.(new Error('Sign-in was cancelled.'))
    this.cancelSignIn = null
    this.link = null
    this.state = null
    this.changed()
  }
}

export const inviteJoin = new InviteJoin()
