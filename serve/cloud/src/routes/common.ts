// Shared pieces of the /v1 handlers: errors, request context, body validation, caller auth and
// the grant/relay helpers several route groups use.

import {
  computerIdFor,
  fromB64url,
  NONCE_TTL_MS,
  clerkSessionActive,
  verifyClerk,
  webOrigins,
  verifySig,
  type SignedRequest,
} from "../auth";
import { ERROR_STATUS, type AccessState, type CloudComputer, type ComputerAccess, type ComputerRole, type ErrorCode } from "../contract.gen";
import type { Env } from "../index";

const MAX_BODY = 64 * 1024;

// ---- errors ----

// Codes and statuses are the cloud contract's (serve/contract/cloud/contract.json): a code it doesn't name won't compile.
export class ApiError extends Error {
  readonly status: number;
  constructor(
    readonly code: ErrorCode,
    message?: string,
    status?: number,
  ) {
    super(message ?? code);
    this.status = status ?? ERROR_STATUS[code];
  }
}

// ---- request context ----

export interface Ctx {
  req: Request;
  env: Env;
  exec: ExecutionContext;
  url: URL;
  /** Raw body bytes: Kith-Sig hashes exactly these. */
  raw: Uint8Array;
  now: number;
}

type Body = Record<string, unknown>;

export function body(c: Ctx): Body {
  if (c.raw.length > MAX_BODY) throw new ApiError("badRequest", "Body too large");
  if (!c.raw.length) return {};
  let v: unknown;
  try {
    v = JSON.parse(new TextDecoder().decode(c.raw));
  } catch {
    throw new ApiError("badRequest", "Body is not JSON");
  }
  if (!v || typeof v !== "object" || Array.isArray(v)) throw new ApiError("badRequest", "Body must be an object");
  return v as Body;
}

export function str(b: Body, key: string, max = 100): string {
  const v = b[key];
  if (typeof v !== "string" || !v.trim() || v.length > max) throw new ApiError("badRequest", `Invalid ${key}`);
  return v;
}

export function optStr(b: Body, key: string, max = 100): string | null {
  return b[key] === undefined || b[key] === null ? null : str(b, key, max);
}

export function oneOf<T extends string>(b: Body, key: string, allowed: readonly T[]): T {
  const v = b[key];
  if (!allowed.includes(v as T)) throw new ApiError("badRequest", `Invalid ${key}`);
  return v as T;
}

/** A base64url field of exactly `len` bytes; returns the original string. */
export function key(b: Body, name: string, len: number): string {
  if (!fromB64url(b[name], len)) throw new ApiError("badRequest", `Invalid ${name}`);
  return b[name] as string;
}

// ---- auth ----

interface User {
  userId: string;
  email: string | null;
  createdAt: number;
  /** The Clerk session and the origin its token was minted for (web devices bind to them). */
  sid: string;
  azp: string | null;
}

export async function user(c: Ctx): Promise<User> {
  const clerk = await verifyClerk(c.req, c.env);
  if (!clerk) throw new ApiError("unauthenticated");
  const row = await c.env.DB.prepare(
    `INSERT INTO accounts(user_id, email, created_at) VALUES (?1, ?2, ?3)
     ON CONFLICT(user_id) DO UPDATE SET email = COALESCE(excluded.email, accounts.email)
     RETURNING status, email, created_at`,
  )
    .bind(clerk.userId, clerk.email, c.now)
    .first<{ status: string; email: string | null; created_at: number }>();
  if (!row || row.status === "deleted") throw new ApiError("accountDeleted");
  return { userId: clerk.userId, email: row.email, createdAt: row.created_at, sid: clerk.sid, azp: clerk.azp };
}

/**
 * A web device is only as good as its Clerk session: the token must have been minted for one of WEB_ORIGINS and Clerk must
 * say the session is live right now (the signature and `exp` alone don't). Fails closed.
 */
export async function webSessionLive(env: Env, clerk: { sid: string; azp: string | null }): Promise<boolean> {
  return !!clerk.azp && webOrigins(env).includes(clerk.azp) && (await clerkSessionActive(clerk.sid, env));
}

/** Kith-Sig with D1 replay protection (§5). */
export async function signed(c: Ctx): Promise<SignedRequest> {
  const s = await verifySig(c.req, c.raw, c.now);
  if (!s) throw new ApiError("badSignature");
  const r = await c.env.DB.prepare("INSERT OR IGNORE INTO sig_nonces(kid, nonce, expires_at) VALUES (?, ?, ?)")
    .bind(s.kid, s.nonce, c.now + NONCE_TTL_MS)
    .run();
  if (r.meta.changes !== 1) throw new ApiError("badSignature");
  return s;
}

export interface ComputerRow {
  id: string;
  sign_pub: string;
  box_pub: string;
  owner_user_id: string | null;
  name: string;
  platform: string;
  device: string | null;
  version: string;
  status: string;
  online: number;
  last_seen_at: number | null;
  claimed_at: number | null;
}

/** Sig(host): the signing key names the computer; it must be registered and not blocked. */
export async function host(c: Ctx): Promise<ComputerRow> {
  const s = await signed(c);
  const id = await computerIdFor(fromB64url(s.kid, 32)!);
  const row = await c.env.DB.prepare("SELECT * FROM computers WHERE id = ?").bind(id).first<ComputerRow>();
  if (!row) throw new ApiError("unknownComputer");
  if (row.status !== "active") throw new ApiError("forbidden");
  return row;
}

/** Sig(dev) for a device registered (and not revoked) under `userId`. */
export async function device(c: Ctx, userId: string): Promise<{ id: string; dk: string }> {
  const s = await signed(c);
  const row = await c.env.DB.prepare(
    "SELECT id FROM devices WHERE owner_user_id = ? AND sign_pub = ? AND revoked_at IS NULL",
  )
    .bind(userId, s.kid)
    .first<{ id: string }>();
  if (!row) throw new ApiError("notFound");
  return { id: row.id, dk: s.kid };
}

export async function ownedComputer(c: Ctx, id: string, userId: string): Promise<ComputerRow> {
  const row = await c.env.DB.prepare("SELECT * FROM computers WHERE id = ? AND owner_user_id = ?")
    .bind(id, userId)
    .first<ComputerRow>();
  if (!row) throw new ApiError("notFound");
  return row;
}

/** The computer for its owner, or for an account with an active membership on it (members may ask for device access). */
export async function memberComputer(c: Ctx, id: string, userId: string): Promise<ComputerRow> {
  const row = await c.env.DB.prepare(
    `SELECT c.* FROM computers c WHERE c.id = ?1 AND (c.owner_user_id = ?2 OR (c.owner_user_id IS NOT NULL AND EXISTS(
       SELECT 1 FROM memberships m WHERE m.computer_id = c.id AND m.user_id = ?2 AND m.revoked_at IS NULL)))`,
  )
    .bind(id, userId)
    .first<ComputerRow>();
  if (!row) throw new ApiError("notFound");
  return row;
}

// ---- shared pieces ----

export function computerView(r: ComputerRow & { access?: ComputerAccess | null; role?: ComputerRole }): CloudComputer {
  return {
    role: r.role ?? "owner",
    computerId: r.id,
    name: r.name,
    platform: r.platform,
    device: r.device,
    signKey: r.sign_pub,
    boxKey: r.box_pub,
    version: r.version,
    online: r.online === 1,
    lastSeenAt: r.last_seen_at,
    claimedAt: r.claimed_at,
    access: r.access ?? null,
  };
}

export const audit = (env: Env, actor: string, action: string, target: string | null, result: string, now: number) =>
  env.DB.prepare("INSERT INTO audit_events(actor, action, target, result, created_at) VALUES (?, ?, ?, ?, ?)").bind(
    actor,
    action,
    target,
    result,
    now,
  );

interface Blocked {
  dk: string;
  grantId: string;
}

/** Statements revoking every active grant matching `filter`, and the DO blocks they imply, per computer. */
export async function revokeGrants(
  env: Env,
  filter: string,
  binds: unknown[],
  reason: string,
  now: number,
): Promise<{ stmts: D1PreparedStatement[]; blocks: Map<string, Blocked[]> }> {
  const { results } = await env.DB.prepare(
    `SELECT g.id, g.computer_id, d.sign_pub FROM grants g JOIN devices d ON d.id = g.device_id
     WHERE g.status = 'active' AND (${filter})`,
  )
    .bind(...binds)
    .all<{ id: string; computer_id: string; sign_pub: string }>();
  const blocks = new Map<string, Blocked[]>();
  const stmts = results.map((g) => {
    blocks.set(g.computer_id, [...(blocks.get(g.computer_id) ?? []), { dk: g.sign_pub, grantId: g.id }]);
    return env.DB.prepare(
      "UPDATE grants SET status = 'revoked', revoked_at = ?, revoked_reason = ? WHERE id = ? AND status = 'active'",
    ).bind(now, reason, g.id);
  });
  return { stmts, blocks };
}

/** Ends one account's membership of a computer: its devices' grants and pending requests go with it. */
export async function revokeMembership(env: Env, computerId: string, userId: string, actor: string, target: string, now: number) {
  const { stmts, blocks } = await revokeGrants(env, "g.computer_id = ? AND d.owner_user_id = ?", [computerId, userId], "membershipRevoked", now);
  await env.DB.batch([
    ...stmts,
    env.DB.prepare("UPDATE memberships SET revoked_at = ? WHERE computer_id = ? AND user_id = ? AND revoked_at IS NULL").bind(
      now,
      computerId,
      userId,
    ),
    env.DB.prepare(
      "UPDATE access_requests SET status = 'cancelled', decided_at = ? WHERE computer_id = ? AND user_id = ? AND status = 'pending'",
    ).bind(now, computerId, userId),
    audit(env, actor, "membership.revoke", target, "ok", now),
  ]);
  await notify(env, computerId, blocks.get(computerId));
}

/** Tells a computer's DO: block these devices (closing their sockets), then `cloud.changed` to the host. */
export async function notify(env: Env, computerId: string, blocked: Blocked[] = []): Promise<void> {
  const path = blocked.length ? "/internal/block" : "/internal/changed";
  try {
    const res = await env.RELAY.get(env.RELAY.idFromName(computerId)).fetch(`https://do${path}`, {
      method: "POST",
      headers: { "X-Kith-Internal": "1", "Content-Type": "application/json" },
      body: JSON.stringify(blocked.length ? { devices: blocked } : {}),
    });
    if (!res.ok) console.error("relay notify failed", { computerId, path, status: res.status });
  } catch (e) {
    console.error("relay notify failed", { computerId, path, error: String(e) });
  }
}

/**
 * Ends everything a Clerk account holds as a member: its devices' grants, its memberships, devices and pending
 * requests. Shared by user.deleted and a ban so both cut the same access. Computers the account owns are not touched
 * here (and neither are bot guests, which hang off a computer's owner, never off a member).
 */
export async function revokeMemberAccess(env: Env, userId: string, reason: string, actor: string, now: number) {
  const { stmts, blocks } = await revokeGrants(env, "d.owner_user_id = ?", [userId], reason, now);
  const memberOf = await env.DB.prepare("SELECT DISTINCT computer_id FROM memberships WHERE user_id = ? AND revoked_at IS NULL")
    .bind(userId)
    .all<{ computer_id: string }>();
  const batch = [
    ...stmts,
    env.DB.prepare("UPDATE memberships SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL").bind(now, userId),
    env.DB.prepare("UPDATE devices SET revoked_at = ? WHERE owner_user_id = ? AND revoked_at IS NULL").bind(now, userId),
    env.DB.prepare("UPDATE access_requests SET status = 'cancelled', decided_at = ? WHERE status = 'pending' AND user_id = ?").bind(
      now,
      userId,
    ),
    ...(actor ? [audit(env, actor, reason, userId, "ok", now)] : []),
  ];
  const computers = [...new Set([...blocks.keys(), ...memberOf.results.map((r) => r.computer_id)])];
  return { batch, blocks, computers };
}

/** True when the account owns a computer: such an account is never revoked by a ban (the owner's call, not ours). */
export async function ownsComputers(env: Env, userId: string): Promise<boolean> {
  return !!(await env.DB.prepare("SELECT 1 FROM computers WHERE owner_user_id = ? LIMIT 1").bind(userId).first());
}

/** Removes a computer from its account: grants revoked, pending requests cancelled, sockets closed. */
export async function unclaim(env: Env, id: string, actor: string, now: number): Promise<void> {
  const { stmts, blocks } = await revokeGrants(env, "g.computer_id = ?", [id], "unclaimed", now);
  // A bot's access hangs off the Mac's owner like a member's, so it ends too: open invites and joined devices.
  const { results: bots } = await env.DB.prepare("SELECT bot_device_id, sign_pub FROM bot_devices WHERE computer_id = ? AND revoked_at IS NULL")
    .bind(id)
    .all<{ bot_device_id: string; sign_pub: string }>();
  const blocked = [...(blocks.get(id) ?? []), ...bots.map((d) => ({ dk: d.sign_pub, grantId: `bot:${d.bot_device_id}` }))];
  // A Mac link needs both computers in one account, so every pairing this computer is on ends too.
  const { results: peers } = await env.DB.prepare(
    "SELECT id, from_computer_id, to_computer_id, device_key FROM peer_grants WHERE status = 'active' AND (from_computer_id = ?1 OR to_computer_id = ?1)",
  )
    .bind(id)
    .all<{ id: string; from_computer_id: string; to_computer_id: string; device_key: string }>();
  for (const g of peers) if (g.to_computer_id === id) blocked.push({ dk: g.device_key, grantId: g.id });
  await env.DB.batch([
    ...stmts,
    env.DB.prepare(
      "UPDATE peer_grants SET status = 'revoked', revoked_at = ?1, revoked_reason = 'unclaimed' WHERE status = 'active' AND (from_computer_id = ?2 OR to_computer_id = ?2)",
    ).bind(now, id),
    env.DB.prepare(
      "UPDATE peer_requests SET status = 'cancelled', decided_at = ?1 WHERE status = 'pending' AND (from_computer_id = ?2 OR to_computer_id = ?2)",
    ).bind(now, id),
    env.DB.prepare("UPDATE computers SET owner_user_id = NULL, claimed_at = NULL, updated_at = ? WHERE id = ?").bind(now, id),
    env.DB.prepare(
      "UPDATE access_requests SET status = 'cancelled', decided_at = ? WHERE computer_id = ? AND status = 'pending'",
    ).bind(now, id),
    // Members and unused invites belong to the account that owned the computer, so they end with it.
    env.DB.prepare("UPDATE memberships SET revoked_at = ? WHERE computer_id = ? AND revoked_at IS NULL").bind(now, id),
    env.DB.prepare("UPDATE invites SET revoked_at = ? WHERE computer_id = ? AND accepted_by IS NULL AND revoked_at IS NULL").bind(now, id),
    env.DB.prepare("UPDATE bot_invites SET revoked_at = ? WHERE computer_id = ? AND accepted_at IS NULL AND revoked_at IS NULL").bind(now, id),
    env.DB.prepare("UPDATE bot_devices SET revoked_at = ? WHERE computer_id = ? AND revoked_at IS NULL").bind(now, id),
    audit(env, actor, "computer.unclaim", id, "ok", now),
  ]);
  await notify(env, id, blocked);
  // The other end of each pairing: an answering computer closes the asker's socket, an asking one re-reads its state.
  for (const g of peers) {
    if (g.to_computer_id === id) await notify(env, g.from_computer_id);
    else await notify(env, g.to_computer_id, [{ dk: g.device_key, grantId: g.id }]);
  }
}


export interface RequestRow {
  id: string;
  computer_id: string;
  device_id: string;
  status: AccessState;
  host_nonce: string | null;
  device_nonce: string | null;
  expires_at: number;
  grant_id: string | null;
}

export const effectiveStatus = (r: RequestRow, now: number): AccessState => (r.status === "pending" && r.expires_at <= now ? "expired" : r.status);
