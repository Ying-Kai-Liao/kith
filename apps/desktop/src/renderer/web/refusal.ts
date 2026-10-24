/**
 * After the cloud refuses a user's device key (retired or revoked, a 403 on register) the page must not quietly make and
 * register a new one: that would undo a sign-out made elsewhere. A new key comes only from an explicit reload (this state
 * lives in memory, so a reload clears it).
 */
export class KeyRefusal {
  private user: string | null = null

  refuse(userId: string) {
    this.user = userId
  }

  /** Whether a missing key may be created for this user in this page. */
  mayCreate(userId: string): boolean {
    return this.user !== userId
  }
}


/**
 * The key lookups the cloud client and the relay use, both behind one KeyRefusal, so neither can make and register a new
 * key for a user the cloud just refused. `refused` is what the relay calls on a 403: it remembers, then drops the key.
 */
export function refusingKeys<S>(store: { identity(userId: string, create: boolean): Promise<S | null>; dropKey(userId: string): Promise<void> }, currentUser: () => string | null) {
  const refusal = new KeyRefusal()
  return {
    cloudSigner: async (): Promise<S | null> => {
      const user = currentUser()
      return user ? store.identity(user, refusal.mayCreate(user)) : null
    },
    relaySigner: async (create: boolean): Promise<S | null> => {
      const user = currentUser()
      return user ? store.identity(user, create && refusal.mayCreate(user)) : null
    },
    refused: async (userId: string) => {
      refusal.refuse(userId)
      await store.dropKey(userId)
    },
  }
}
