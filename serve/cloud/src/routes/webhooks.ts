// Clerk webhooks: a deleted user takes their account, devices, grants and computers with them; a banned or locked
// member loses their access.

import { Webhook } from "svix";
import type { Env } from "../index";
import { ApiError, audit, notify, ownsComputers, revokeGrants, revokeMemberAccess, type Ctx } from "./common";
import type { Responses } from "../contract.gen";

export async function clerkWebhook(c: Ctx): Promise<Responses["clerkWebhook"]> {
  if (!c.env.CLERK_WEBHOOK_SECRET) throw new ApiError("internal", "Webhook secret not configured");
  const payload = new TextDecoder().decode(c.raw);
  const h = (name: string) => c.req.headers.get(name) ?? "";
  let event: { type?: string; data?: { id?: unknown; banned?: unknown; locked?: unknown } } | null;
  try {
    // Throws unless the signature and timestamp check out; returns nothing.
    new Webhook(c.env.CLERK_WEBHOOK_SECRET).verify(payload, {
      "svix-id": h("svix-id"),
      "svix-timestamp": h("svix-timestamp"),
      "svix-signature": h("svix-signature"),
    });
    event = JSON.parse(payload);
  } catch {
    throw new ApiError("badSignature");
  }
  const seen = await c.env.DB.prepare("SELECT 1 FROM processed_events WHERE provider = 'clerk' AND event_id = ?")
    .bind(h("svix-id"))
    .first();
  if (seen) return {};
  const userId = event?.data?.id;
  if (typeof userId === "string") {
    if (event?.type === "user.deleted") await deleteAccount(c.env, userId, c.now);
    else if (event?.type === "user.updated" && (event.data?.banned === true || event.data?.locked === true))
      await banAccount(c.env, userId, event.data.banned === true ? "accountBanned" : "accountLocked", c.now);
    // session.revoked is ignored on purpose: devices carry their own keys and no Clerk session id, so one signed-out
    // session cannot be tied to a device, and revoking the whole account for it would be wrong.
  }
  await c.env.DB.prepare("INSERT OR IGNORE INTO processed_events(provider, event_id, processed_at) VALUES ('clerk', ?, ?)")
    .bind(h("svix-id"), c.now)
    .run();
  return {};
}

/** A banned or locked member loses all access. An owner (an account with computers) is left as is and logged. */
async function banAccount(env: Env, userId: string, reason: string, now: number): Promise<void> {
  if (await ownsComputers(env, userId)) {
    console.warn("clerk ban of a host owner left untouched", { userId, reason });
    return;
  }
  const { batch, blocks, computers } = await revokeMemberAccess(env, userId, reason, "system", now);
  await env.DB.batch(batch);
  await Promise.all(computers.map((id) => notify(env, id, blocks.get(id))));
}

async function deleteAccount(env: Env, userId: string, now: number): Promise<void> {
  const member = await revokeMemberAccess(env, userId, "accountDeleted", "", now);
  // The member part covers the account's own devices; this covers other devices' grants on the computers it leaves.
  const { stmts, blocks } = await revokeGrants(
    env,
    "g.computer_id IN (SELECT id FROM computers WHERE owner_user_id = ?1) AND d.owner_user_id != ?1",
    [userId],
    "accountDeleted",
    now,
  );
  for (const [id, list] of member.blocks) blocks.set(id, [...(blocks.get(id) ?? []), ...list]);
  const owned = await env.DB.prepare("SELECT id FROM computers WHERE owner_user_id = ?").bind(userId).all<{ id: string }>();
  // Bot guests hang off the owner like members, so they leave with the computers (as unclaim does).
  const bots = await env.DB.prepare(
    "SELECT bot_device_id, computer_id, sign_pub FROM bot_devices WHERE revoked_at IS NULL AND computer_id IN (SELECT id FROM computers WHERE owner_user_id = ?)",
  )
    .bind(userId)
    .all<{ bot_device_id: string; computer_id: string; sign_pub: string }>();
  for (const b of bots.results)
    blocks.set(b.computer_id, [...(blocks.get(b.computer_id) ?? []), { dk: b.sign_pub, grantId: `bot:${b.bot_device_id}` }]);
  const mine = "computer_id IN (SELECT id FROM computers WHERE owner_user_id = ?2)";
  await env.DB.batch([
    ...member.batch,
    ...stmts,
    env.DB.prepare(
      `INSERT INTO accounts(user_id, status, created_at, deleted_at) VALUES (?1, 'deleted', ?2, ?2)
       ON CONFLICT(user_id) DO UPDATE SET status = 'deleted', deleted_at = ?2`,
    ).bind(userId, now),
    // Everyone's memberships and unused invites on the computers they leave.
    env.DB.prepare(`UPDATE memberships SET revoked_at = ?1 WHERE revoked_at IS NULL AND ${mine}`).bind(now, userId),
    env.DB.prepare(`UPDATE invites SET revoked_at = ?1 WHERE accepted_by IS NULL AND revoked_at IS NULL AND ${mine}`).bind(now, userId),
    env.DB.prepare(`UPDATE bot_invites SET revoked_at = ?1 WHERE accepted_at IS NULL AND revoked_at IS NULL AND ${mine}`).bind(now, userId),
    env.DB.prepare(`UPDATE bot_devices SET revoked_at = ?1 WHERE revoked_at IS NULL AND ${mine}`).bind(now, userId),
    env.DB.prepare(`UPDATE access_requests SET status = 'cancelled', decided_at = ?1 WHERE status = 'pending' AND ${mine}`).bind(now, userId),
    env.DB.prepare("UPDATE computers SET owner_user_id = NULL, claimed_at = NULL, updated_at = ? WHERE owner_user_id = ?").bind(
      now,
      userId,
    ),
    audit(env, "system", "account.delete", userId, "ok", now),
  ]);
  const computers = new Set([...blocks.keys(), ...owned.results.map((r) => r.id), ...member.computers]);
  await Promise.all([...computers].map((id) => notify(env, id, blocks.get(id))));
}
