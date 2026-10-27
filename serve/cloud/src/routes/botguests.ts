// Bot guests: a cloud agent (Grok Bot and the like) joins a Mac with a link. The Mac posts the invite, the bot's own
// device key accepts it (Kith-Sig, no Clerk account), and the cloud records "this key accepted that invite". The Mac
// decides what that means and the cloud can only remove access (docs/kith-bot-guests.md, sections 3 and 5.3).

import { randomId, sha256, utf8 } from "../auth";
import { ApiError, audit, body, host, notify, optStr, signed, str, type Ctx } from "./common";
import type { Responses } from "../contract.gen";

/** Same ceiling as member invites: the Mac makes 24 h links, a longer ask is a bug or a hostile host. */
const MAX_INVITE_TTL_MS = 48 * 3600_000;
const MAX_BOT_DEVICES = 50;
const MAX_OPEN_INVITES = 100;
const HEX64 = /^[0-9a-f]{64}$/;
const INVITE_ID = /^[A-Za-z0-9_-]{1,64}$/;
const CONTROL = /\p{Cc}/u;

interface BotInviteRow {
  invite_id: string;
  computer_id: string;
  token_hash: string;
  expires_at: number;
  accepted_at: number | null;
  revoked_at: number | null;
}

interface BotDeviceRow {
  bot_device_id: string;
  computer_id: string;
  invite_id: string;
  sign_pub: string;
  revoked_at: number | null;
}

const hex = (bytes: Uint8Array) => Array.from(bytes, (x) => x.toString(16).padStart(2, "0")).join("");

// ---- host-signed ----

export async function hostCreateBotInvite(c: Ctx): Promise<Responses["hostCreateBotInvite"]> {
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
  // No claim needed: a bot has no account, so the invite doesn't depend on one.
  const inserted = await c.env.DB.prepare(
    `INSERT OR IGNORE INTO bot_invites(invite_id, computer_id, token_hash, expires_at, created_at)
     SELECT ?1, ?2, ?3, ?4, ?5
     WHERE (SELECT COUNT(*) FROM bot_invites WHERE computer_id = ?2 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > ?5) < ?6`,
  )
    .bind(inviteId, comp.id, tokenHash, expiresAt, c.now, MAX_OPEN_INVITES)
    .run();
  if (inserted.meta.changes === 0) {
    // Re-posting the same invite is a no-op; the same id with another hash or computer is a clash.
    const row = await c.env.DB.prepare("SELECT * FROM bot_invites WHERE invite_id = ?").bind(inviteId).first<BotInviteRow>();
    if (!row) throw new ApiError("tooMany", "Too many open bot invites on this computer");
    if (row.computer_id !== comp.id || row.token_hash !== tokenHash) throw new ApiError("conflict", "inviteId is taken");
    return { ok: true };
  }
  await audit(c.env, `computer:${comp.id}`, "botinvite.create", inviteId, "ok", c.now).run();
  return { ok: true };
}

export async function hostRevokeBotInvite(c: Ctx, [inviteId]: string[]): Promise<Responses["hostRevokeBotInvite"]> {
  const comp = await host(c);
  const row = await c.env.DB.prepare("SELECT * FROM bot_invites WHERE invite_id = ? AND computer_id = ?")
    .bind(inviteId, comp.id)
    .first<BotInviteRow>();
  if (!row) throw new ApiError("notFound");
  if (row.accepted_at) throw new ApiError("conflict", "The invite was accepted; revoke the bot device instead");
  if (row.revoked_at) return { ok: true };
  const updated = await c.env.DB.prepare(
    "UPDATE bot_invites SET revoked_at = ? WHERE invite_id = ? AND accepted_at IS NULL AND revoked_at IS NULL",
  )
    .bind(c.now, inviteId)
    .run();
  // The bot accepted between the read and the write: the invite is no longer the thing to revoke (and nothing is audited).
  if (updated.meta.changes === 0) throw new ApiError("conflict", "The invite was accepted; revoke the bot device instead");
  await audit(c.env, `computer:${comp.id}`, "botinvite.revoke", inviteId!, "ok", c.now).run();
  return { ok: true };
}

/** Ends one bot device and closes its relay sockets. Repeating it re-sends the block, so a lost notify can be retried. */
export async function hostRevokeBotDevice(c: Ctx, [botDeviceId]: string[]): Promise<Responses["hostRevokeBotDevice"]> {
  const comp = await host(c);
  const row = await c.env.DB.prepare("SELECT * FROM bot_devices WHERE bot_device_id = ? AND computer_id = ?")
    .bind(botDeviceId, comp.id)
    .first<BotDeviceRow>();
  if (!row) throw new ApiError("notFound");
  if (!row.revoked_at) {
    await c.env.DB.batch([
      c.env.DB.prepare("UPDATE bot_devices SET revoked_at = ? WHERE bot_device_id = ? AND revoked_at IS NULL").bind(c.now, botDeviceId),
      audit(c.env, `computer:${comp.id}`, "botdevice.revoke", botDeviceId!, "ok", c.now),
    ]);
  }
  // A re-sent revoke for an old device must not block the key while a newer device (a rejoin with the same key) holds it.
  const newer = await c.env.DB.prepare("SELECT 1 FROM bot_devices WHERE computer_id = ? AND sign_pub = ? AND revoked_at IS NULL")
    .bind(comp.id, row.sign_pub)
    .first();
  await notify(c.env, comp.id, newer ? [] : [{ dk: row.sign_pub, grantId: `bot:${botDeviceId}` }]);
  return { ok: true };
}

/** The active bot devices of a computer, for `GET /v1/host/state`. */
export async function activeBotDevices(c: Ctx, computerId: string) {
  const { results } = await c.env.DB.prepare(
    `SELECT bot_device_id, invite_id, sign_pub, name, provider, created_at FROM bot_devices
     WHERE computer_id = ? AND revoked_at IS NULL ORDER BY created_at, bot_device_id`,
  )
    .bind(computerId)
    .all<{ bot_device_id: string; invite_id: string; sign_pub: string; name: string; provider: string | null; created_at: number }>();
  return results.map((d) => ({
    botDeviceId: d.bot_device_id,
    inviteId: d.invite_id,
    deviceKey: d.sign_pub,
    name: d.name,
    provider: d.provider,
    createdAt: d.created_at,
  }));
}

// ---- bot-signed ----

/**
 * The bot (Kith-Sig by its own key) joins the computer whose Mac made this invite. Single use and bound to the first
 * key to accept; the same key repeating it gets the same device, so a retry or a second `kith join` is safe.
 */
export async function acceptBotInvite(c: Ctx, [inviteId]: string[]): Promise<Responses["acceptBotInvite"]> {
  // Before the signature check, so a flood of bad signatures can't skip the limit (each one costs a nonce row).
  if (c.env.BOT_JOIN_LIMITER) {
    const ip = c.req.headers.get("CF-Connecting-IP") ?? "unknown";
    if (!(await c.env.BOT_JOIN_LIMITER.limit({ key: ip })).success) throw new ApiError("rateLimited");
  }
  const s = await signed(c);
  const b = body(c);
  const token = str(b, "token", 64);
  if (!HEX64.test(token)) throw new ApiError("badRequest", "Invalid token");
  const name = str(b, "name", 64);
  const provider = optStr(b, "provider", 32);
  // The name is shown to the Mac's owner in a prompt and printed in logs.
  if (CONTROL.test(name) || (provider !== null && CONTROL.test(provider))) throw new ApiError("badRequest", "Invalid name");
  const inv = await c.env.DB.prepare("SELECT * FROM bot_invites WHERE invite_id = ?").bind(inviteId).first<BotInviteRow>();
  // An unknown invite and a wrong token answer alike, so invite ids can't be probed for existence.
  if (!inv || hex(await sha256(utf8(token))) !== inv.token_hash) throw new ApiError("forbidden", "This invite link isn't valid");
  const comp = await c.env.DB.prepare("SELECT status FROM computers WHERE id = ?").bind(inv.computer_id).first<{ status: string }>();
  if (comp?.status !== "active") throw new ApiError("inviteRevoked", "This invite is no longer valid");

  const device = () => c.env.DB.prepare("SELECT * FROM bot_devices WHERE invite_id = ?").bind(inviteId).first<BotDeviceRow>();
  /** This key's own device on the invite, if it is the one that accepted. */
  const mine = (d: BotDeviceRow | null) => {
    if (d?.sign_pub !== s.kid) return null;
    // Access ended after joining: a used invite doesn't bring it back, the owner invites again.
    if (d.revoked_at) throw new ApiError("inviteRevoked", "Your access to this computer was removed");
    return { computerId: inv.computer_id, botDeviceId: d.bot_device_id };
  };
  const used = () => new ApiError("inviteUsed", "This invite was already used");

  if (inv.accepted_at) {
    const again = mine(await device());
    if (again) return again;
    throw used();
  }
  if (inv.revoked_at) throw new ApiError("inviteRevoked", "This invite was cancelled");
  if (inv.expires_at <= c.now) throw new ApiError("inviteExpired", "This invite has expired");

  const botDeviceId = randomId("bd_");
  // One winner when two clients race. The device is inserted only while the invite is still open (and under the cap),
  // and the invite is marked accepted only if that insert made this key's device, all in one transaction. A key that
  // already holds an active device on this computer hits the unique index and leaves the invite open.
  const [made] = await c.env.DB.batch([
    c.env.DB.prepare(
      `INSERT INTO bot_devices(bot_device_id, computer_id, invite_id, sign_pub, name, provider, created_at)
       SELECT ?1, computer_id, invite_id, ?2, ?3, ?4, ?5 FROM bot_invites
       WHERE invite_id = ?6 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > ?5
         AND (SELECT COUNT(*) FROM bot_devices WHERE computer_id = bot_invites.computer_id AND revoked_at IS NULL) < ?7
       ON CONFLICT DO NOTHING`,
    ).bind(botDeviceId, s.kid, name, provider, c.now, inviteId, MAX_BOT_DEVICES),
    c.env.DB.prepare(
      `UPDATE bot_invites SET accepted_at = ?1
       WHERE invite_id = ?2 AND accepted_at IS NULL AND EXISTS(SELECT 1 FROM bot_devices WHERE invite_id = ?2 AND sign_pub = ?3)`,
    ).bind(c.now, inviteId, s.kid),
  ]);
  if (made!.meta.changes === 0) {
    const latest = await c.env.DB.prepare("SELECT * FROM bot_invites WHERE invite_id = ?").bind(inviteId).first<BotInviteRow>();
    if (latest?.revoked_at) throw new ApiError("inviteRevoked", "This invite was cancelled");
    if (latest?.accepted_at) {
      // Our own retry landed first, or somebody else won.
      const again = mine(await device());
      if (again) return again;
      throw used();
    }
    if (latest && latest.expires_at <= c.now) throw new ApiError("inviteExpired", "This invite has expired");
    const active = await c.env.DB.prepare("SELECT 1 FROM bot_devices WHERE computer_id = ? AND sign_pub = ? AND revoked_at IS NULL")
      .bind(inv.computer_id, s.kid)
      .first();
    if (active) throw new ApiError("conflict", "This key already has a bot on this computer");
    throw new ApiError("tooMany", "This computer has too many bots");
  }
  await audit(c.env, `bot:${botDeviceId}`, "botinvite.accept", inviteId!, "ok", c.now).run();
  await notify(c.env, inv.computer_id);
  return { computerId: inv.computer_id, botDeviceId };
}
