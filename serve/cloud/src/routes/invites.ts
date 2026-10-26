// Invites and memberships. The Mac (Sig(host)) posts an invite it made, an account accepts it, and the cloud
// records "account X is a member of computer Y". The host decides what that means; the cloud can only remove access.

import { randomId, sha256, utf8 } from "../auth";
import { ApiError, audit, body, host, notify, revokeMembership, str, user, type Ctx } from "./common";
import type { Responses } from "../contract.gen";

/** The host makes 24 h invites; a longer ask is a bug or a hostile host, not clock skew. */
const MAX_INVITE_TTL_MS = 48 * 3600_000;
const HEX64 = /^[0-9a-f]{64}$/;
const INVITE_ID = /^[A-Za-z0-9_-]{1,64}$/;

interface InviteRow {
  invite_id: string;
  computer_id: string;
  token_hash: string;
  expires_at: number;
  accepted_by: string | null;
  revoked_at: number | null;
}

interface MembershipRow {
  membership_id: string;
  computer_id: string;
  user_id: string;
  revoked_at: number | null;
}

const hex = (bytes: Uint8Array) => Array.from(bytes, (x) => x.toString(16).padStart(2, "0")).join("");

export async function hostCreateInvite(c: Ctx): Promise<Responses["hostCreateInvite"]> {
  const comp = await host(c);
  const b = body(c);
  const inviteId = str(b, "inviteId", 64);
  const tokenHash = str(b, "tokenHash", 64);
  const expiresAt = b.expiresAt;
  if (!INVITE_ID.test(inviteId)) throw new ApiError("badRequest", "Invalid inviteId");
  if (!HEX64.test(tokenHash)) throw new ApiError("badRequest", "Invalid tokenHash");
  if (!Number.isSafeInteger(expiresAt) || (expiresAt as number) <= c.now || (expiresAt as number) > c.now + MAX_INVITE_TTL_MS) {
    throw new ApiError("badRequest", "Invalid expiresAt");
  }
  // Invitees join an account's computer; an unclaimed one has no account to invite them into.
  if (!comp.owner_user_id) throw new ApiError("forbidden", "Sign in on the computer to invite people");
  const inserted = await c.env.DB.prepare(
    "INSERT OR IGNORE INTO invites(invite_id, computer_id, token_hash, expires_at, created_at) VALUES (?, ?, ?, ?, ?)",
  )
    .bind(inviteId, comp.id, tokenHash, expiresAt, c.now)
    .run();
  if (inserted.meta.changes === 0) {
    // Re-posting the same invite is a no-op; the same id with another hash or computer is a clash.
    const row = await c.env.DB.prepare("SELECT * FROM invites WHERE invite_id = ?").bind(inviteId).first<InviteRow>();
    if (!row || row.computer_id !== comp.id || row.token_hash !== tokenHash) throw new ApiError("conflict", "inviteId is taken");
    return { ok: true };
  }
  await audit(c.env, `computer:${comp.id}`, "invite.create", inviteId, "ok", c.now).run();
  return { ok: true };
}

export async function hostRevokeInvite(c: Ctx, [inviteId]: string[]): Promise<Responses["hostRevokeInvite"]> {
  const comp = await host(c);
  const row = await c.env.DB.prepare("SELECT * FROM invites WHERE invite_id = ? AND computer_id = ?")
    .bind(inviteId, comp.id)
    .first<InviteRow>();
  if (!row) throw new ApiError("notFound");
  if (row.accepted_by) throw new ApiError("conflict", "The invite was accepted; revoke the membership instead");
  if (row.revoked_at) return { ok: true };
  await c.env.DB.batch([
    c.env.DB.prepare("UPDATE invites SET revoked_at = ? WHERE invite_id = ? AND accepted_by IS NULL AND revoked_at IS NULL").bind(
      c.now,
      inviteId,
    ),
    audit(c.env, `computer:${comp.id}`, "invite.revoke", inviteId!, "ok", c.now),
  ]);
  return { ok: true };
}

export async function hostRevokeMembership(c: Ctx, [membershipId]: string[]): Promise<Responses["hostRevokeMembership"]> {
  const comp = await host(c);
  const m = await c.env.DB.prepare("SELECT * FROM memberships WHERE membership_id = ? AND computer_id = ?")
    .bind(membershipId, comp.id)
    .first<MembershipRow>();
  if (!m) throw new ApiError("notFound");
  if (m.revoked_at) return { ok: true };
  await revokeMembership(c.env, comp.id, m.user_id, `computer:${comp.id}`, membershipId!, c.now);
  return { ok: true };
}

/** Every computer the account `ownerUserId` owns, for a change that holds on all of them. */
async function ownerComputers(c: Ctx, ownerUserId: string): Promise<string[]> {
  const rows = await c.env.DB.prepare("SELECT id FROM computers WHERE owner_user_id = ?").bind(ownerUserId).all<{ id: string }>();
  return rows.results.map((r) => r.id);
}

/** The account a host names in a person revoke or undo: one that exists and isn't the computer's owner. */
async function revokeTarget(c: Ctx): Promise<{ owner: string; userId: string }> {
  const comp = await host(c);
  const userId = str(body(c), "userId", 64);
  if (!comp.owner_user_id) throw new ApiError("forbidden", "Sign in on the computer to revoke people");
  if (userId === comp.owner_user_id) throw new ApiError("badRequest", "The owner can't be revoked");
  const known = await c.env.DB.prepare("SELECT 1 FROM accounts WHERE user_id = ?").bind(userId).first();
  if (!known) throw new ApiError("notFound", "No such account");
  return { owner: comp.owner_user_id, userId };
}

/**
 * The owner revoked a person (R6, R7): the account loses its memberships on every computer the owner has (the relay
 * blocks its devices there), may not join one again until undone, and every one of those Macs is told to pull, where
 * `GET /v1/host/state` lists it under `revoked` and the Mac sets the person's revoked state in its own kith-auth.
 * Revoking again is a no-op that still tells the Macs.
 */
export async function hostRevokePerson(c: Ctx): Promise<Responses["hostRevokePerson"]> {
  const { owner, userId } = await revokeTarget(c);
  // The row first: an accept that races this either lands before (and its membership is cut below) or sees the row.
  await c.env.DB.batch([
    c.env.DB.prepare("INSERT OR IGNORE INTO revocations(owner_user_id, user_id, revoked_at) VALUES (?, ?, ?)").bind(owner, userId, c.now),
    audit(c.env, `owner:${owner}`, "person.revoke", userId, "ok", c.now),
  ]);
  for (const computerId of await ownerComputers(c, owner)) {
    const active = await c.env.DB.prepare("SELECT 1 FROM memberships WHERE computer_id = ? AND user_id = ? AND revoked_at IS NULL")
      .bind(computerId, userId)
      .first();
    if (active) await revokeMembership(c.env, computerId, userId, `owner:${owner}`, userId, c.now);
    else await notify(c.env, computerId);
  }
  return { ok: true };
}

/** Undo of a person revoke: the row goes and every Mac of the owner is told; each clears the state in kith-auth. */
export async function hostUnrevokePerson(c: Ctx): Promise<Responses["hostUnrevokePerson"]> {
  const { owner, userId } = await revokeTarget(c);
  await c.env.DB.batch([
    c.env.DB.prepare("DELETE FROM revocations WHERE owner_user_id = ? AND user_id = ?").bind(owner, userId),
    audit(c.env, `owner:${owner}`, "person.unrevoke", userId, "ok", c.now),
  ]);
  for (const computerId of await ownerComputers(c, owner)) await notify(c.env, computerId);
  return { ok: true };
}

/** The accounts the owner of `computerId` revoked, for its host state. */
export async function revokedOf(c: Ctx, ownerUserId: string | null): Promise<string[]> {
  if (!ownerUserId) return [];
  const rows = await c.env.DB.prepare("SELECT user_id FROM revocations WHERE owner_user_id = ? ORDER BY user_id")
    .bind(ownerUserId)
    .all<{ user_id: string }>();
  return rows.results.map((r) => r.user_id);
}

/** The account joins the computer whose Mac made this invite. Single use; the same account may repeat it. */
export async function acceptInvite(c: Ctx, [inviteId]: string[]): Promise<Responses["acceptInvite"]> {
  const u = await user(c);
  const token = str(body(c), "token", 200);
  const inv = await c.env.DB.prepare("SELECT * FROM invites WHERE invite_id = ?").bind(inviteId).first<InviteRow>();
  if (!inv) throw new ApiError("notFound", "This invite doesn't exist");
  if (hex(await sha256(utf8(token))) !== inv.token_hash) throw new ApiError("forbidden", "This invite link isn't valid");
  const comp = await c.env.DB.prepare("SELECT owner_user_id FROM computers WHERE id = ?")
    .bind(inv.computer_id)
    .first<{ owner_user_id: string | null }>();
  if (!comp?.owner_user_id) throw new ApiError("inviteRevoked", "This invite is no longer valid");
  if (comp.owner_user_id === u.userId) throw new ApiError("forbidden", "This is your own computer");
  const revoked = await c.env.DB.prepare("SELECT 1 FROM revocations WHERE owner_user_id = ? AND user_id = ?")
    .bind(comp.owner_user_id, u.userId)
    .first();
  // The owner removed this account's access: no invite of theirs brings it back until they undo it.
  if (revoked) throw new ApiError("inviteRevoked", "Your access to this computer was removed");

  const mine = () =>
    c.env.DB.prepare("SELECT membership_id, revoked_at FROM memberships WHERE invite_id = ? AND user_id = ?")
      .bind(inviteId, u.userId)
      .first<{ membership_id: string; revoked_at: number | null }>();

  if (inv.accepted_by === u.userId) {
    const m = await mine();
    // Access ended after joining: a used invite doesn't bring it back, the owner invites again.
    if (!m || m.revoked_at) throw new ApiError("inviteRevoked", "Your access to this computer was removed");
    return { computerId: inv.computer_id, membershipId: m.membership_id };
  }
  if (inv.revoked_at) throw new ApiError("inviteRevoked", "This invite was cancelled");
  if (inv.accepted_by) throw new ApiError("conflict", "This invite was already used");
  if (inv.expires_at <= c.now) throw new ApiError("inviteExpired", "This invite has expired");
  const already = await c.env.DB.prepare("SELECT 1 FROM memberships WHERE computer_id = ? AND user_id = ? AND revoked_at IS NULL")
    .bind(inv.computer_id, u.userId)
    .first();
  if (already) throw new ApiError("conflict", "You're already a member of this computer");

  const membershipId = randomId("mem_");
  // One winner when two accounts race: only the UPDATE that still sees accepted_by NULL goes on to insert.
  const [won] = await c.env.DB.batch([
    c.env.DB.prepare(
      `UPDATE invites SET accepted_by = ?1, accepted_at = ?2
       WHERE invite_id = ?3 AND accepted_by IS NULL AND revoked_at IS NULL AND expires_at > ?2
         AND NOT EXISTS(SELECT 1 FROM revocations r JOIN computers c ON c.owner_user_id = r.owner_user_id
                        WHERE c.id = invites.computer_id AND r.user_id = ?1)`,
    ).bind(u.userId, c.now, inviteId),
    c.env.DB.prepare(
      `INSERT INTO memberships(membership_id, computer_id, user_id, invite_id, created_at)
       SELECT ?1, computer_id, ?2, invite_id, ?3 FROM invites WHERE invite_id = ?4 AND accepted_by = ?2 AND accepted_at = ?3
       ON CONFLICT DO NOTHING`,
    ).bind(membershipId, u.userId, c.now, inviteId),
  ]);
  if (won!.meta.changes === 0) {
    const latest = await c.env.DB.prepare("SELECT * FROM invites WHERE invite_id = ?").bind(inviteId).first<InviteRow>();
    if (latest?.revoked_at) throw new ApiError("inviteRevoked", "This invite was cancelled");
    const cut = await c.env.DB.prepare("SELECT 1 FROM revocations WHERE owner_user_id = ? AND user_id = ?")
      .bind(comp.owner_user_id, u.userId)
      .first();
    if (cut) throw new ApiError("inviteRevoked", "Your access to this computer was removed");
    if (latest?.accepted_by === u.userId) {
      // Our own double click landed first.
      const m = await mine();
      if (m && !m.revoked_at) return { computerId: inv.computer_id, membershipId: m.membership_id };
    }
    if (latest?.accepted_by) throw new ApiError("conflict", "This invite was already used");
    throw new ApiError("inviteExpired", "This invite has expired");
  }
  const saved = await mine();
  await audit(c.env, u.userId, "invite.accept", inviteId!, "ok", c.now).run();
  await notify(c.env, inv.computer_id);
  return { computerId: inv.computer_id, membershipId: saved?.membership_id ?? membershipId };
}
