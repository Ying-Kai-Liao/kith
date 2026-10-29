// Sig(host) routes of the Mac link (designs/2026-10-29-mac-link): one computer pairs with another computer of the same
// account by the account-device SAS, and the answering computer decides at the Mac. The cloud offers the pair and
// relays the commit and nonces; it never decides one, and it lists a peer grant only while both computers have the
// same owner, so it can shorten a link (an unclaim, an owner change) but never add one.

import { randomId } from "../auth";
import { ApiError, audit, body, host, key, notify, oneOf, str, type ComputerRow, type Ctx } from "./common";
import { DecisionValues, type AccessState, type Responses } from "../contract.gen";

const REQUEST_TTL_MS = 10 * 60_000;
const REQUESTS_PER_HOUR = 20;

interface PeerRequestRow {
  id: string;
  from_computer_id: string;
  to_computer_id: string;
  device_key: string;
  commit_hash: string;
  host_nonce: string | null;
  device_nonce: string | null;
  status: AccessState;
  grant_id: string | null;
  created_at: number;
  expires_at: number;
}

const status = (r: PeerRequestRow, now: number): AccessState => (r.status === "pending" && r.expires_at <= now ? "expired" : r.status);

/** The same account's other computers, which this one may ask to pair with. */
export async function hostPeerComputers(c: Ctx): Promise<Responses["hostPeerComputers"]> {
  const comp = await host(c);
  if (!comp.owner_user_id) return { computers: [] };
  const { results } = await c.env.DB.prepare(
    `SELECT id, name, platform, sign_pub FROM computers
     WHERE owner_user_id = ? AND id != ? AND status = 'active' ORDER BY claimed_at`,
  )
    .bind(comp.owner_user_id, comp.id)
    .all<{ id: string; name: string; platform: string | null; sign_pub: string }>();
  return { computers: results.map((r) => ({ computerId: r.id, name: r.name, platform: r.platform, signKey: r.sign_pub })) };
}

/** A computer of the same owner as `comp`, or notFound. */
async function sibling(c: Ctx, comp: ComputerRow, id: string): Promise<ComputerRow> {
  const to = await c.env.DB.prepare("SELECT * FROM computers WHERE id = ? AND status = 'active'").bind(id).first<ComputerRow>();
  if (!to || !comp.owner_user_id || to.owner_user_id !== comp.owner_user_id || to.id === comp.id) throw new ApiError("notFound");
  return to;
}

export async function hostCreatePeerRequest(c: Ctx): Promise<Responses["hostCreatePeerRequest"]> {
  const comp = await host(c);
  const b = body(c);
  const to = await sibling(c, comp, str(b, "computerId", 64));
  const deviceKey = key(b, "deviceKey", 32);
  const commit = key(b, "commit", 32);
  const recent = await c.env.DB.prepare("SELECT COUNT(*) AS n FROM peer_requests WHERE from_computer_id = ? AND created_at > ?")
    .bind(comp.id, c.now - 3600_000)
    .first<{ n: number }>();
  if ((recent?.n ?? 0) >= REQUESTS_PER_HOUR) throw new ApiError("rateLimited");
  const requestId = randomId("prq_");
  const expiresAt = c.now + REQUEST_TTL_MS;
  // A commit is never reused: an earlier pending request between the two is cancelled, not refreshed.
  await c.env.DB.batch([
    c.env.DB.prepare(
      "UPDATE peer_requests SET status = 'cancelled', decided_at = ? WHERE from_computer_id = ? AND to_computer_id = ? AND status = 'pending'",
    ).bind(c.now, comp.id, to.id),
    c.env.DB.prepare(
      `INSERT INTO peer_requests(id, from_computer_id, to_computer_id, device_key, commit_hash, created_at, expires_at)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
    ).bind(requestId, comp.id, to.id, deviceKey, commit, c.now, expiresAt),
  ]);
  await notify(c.env, to.id);
  return { requestId, expiresAt };
}

async function asked(c: Ctx, comp: ComputerRow, id: string): Promise<PeerRequestRow> {
  const r = await c.env.DB.prepare("SELECT * FROM peer_requests WHERE id = ? AND from_computer_id = ?").bind(id, comp.id).first<PeerRequestRow>();
  if (!r) throw new ApiError("notFound");
  return r;
}

async function answering(c: Ctx, comp: ComputerRow, id: string): Promise<PeerRequestRow> {
  const r = await c.env.DB.prepare("SELECT * FROM peer_requests WHERE id = ? AND to_computer_id = ?").bind(id, comp.id).first<PeerRequestRow>();
  if (!r) throw new ApiError("notFound");
  return r;
}

export async function hostGetPeerRequest(c: Ctx, [id]: string[]): Promise<Responses["hostGetPeerRequest"]> {
  const comp = await host(c);
  const r = await asked(c, comp, id!);
  return {
    requestId: r.id,
    computerId: r.to_computer_id,
    status: status(r, c.now),
    expiresAt: r.expires_at,
    ...(r.host_nonce ? { hostNonce: r.host_nonce } : {}),
    ...(r.grant_id && r.status === "approved" ? { grantId: r.grant_id } : {}),
  };
}

export async function hostRevealPeerRequest(c: Ctx, [id]: string[]): Promise<Responses["hostRevealPeerRequest"]> {
  const comp = await host(c);
  const nonce = key(body(c), "nonce", 32);
  const res = await c.env.DB.prepare(
    `UPDATE peer_requests SET device_nonce = ?
     WHERE id = ? AND from_computer_id = ? AND status = 'pending' AND expires_at > ? AND host_nonce IS NOT NULL AND device_nonce IS NULL`,
  )
    .bind(nonce, id, comp.id, c.now)
    .run();
  if (res.meta.changes === 0) {
    const r = await asked(c, comp, id!);
    if (status(r, c.now) === "expired") throw new ApiError("requestExpired");
    throw new ApiError("conflict", r.host_nonce ? undefined : "The other computer hasn't answered yet");
  }
  const r = await asked(c, comp, id!);
  await notify(c.env, r.to_computer_id);
  return {};
}

export async function hostCancelPeerRequest(c: Ctx, [id]: string[]): Promise<Responses["hostCancelPeerRequest"]> {
  const comp = await host(c);
  const r = await asked(c, comp, id!);
  await c.env.DB.prepare("UPDATE peer_requests SET status = 'cancelled', decided_at = ? WHERE id = ? AND status = 'pending'")
    .bind(c.now, r.id)
    .run();
  await notify(c.env, r.to_computer_id);
  return {};
}

export async function hostPeerNonce(c: Ctx, [id]: string[]): Promise<Responses["hostPeerNonce"]> {
  const comp = await host(c);
  const nonce = key(body(c), "nonce", 32);
  const res = await c.env.DB.prepare(
    `UPDATE peer_requests SET host_nonce = ?
     WHERE id = ? AND to_computer_id = ? AND status = 'pending' AND expires_at > ? AND host_nonce IS NULL`,
  )
    .bind(nonce, id, comp.id, c.now)
    .run();
  if (res.meta.changes === 0) {
    const r = await answering(c, comp, id!);
    if (status(r, c.now) === "expired") throw new ApiError("requestExpired");
    throw new ApiError("conflict");
  }
  return {};
}

export async function hostPeerDecision(c: Ctx, [id]: string[]): Promise<Responses["hostPeerDecision"]> {
  const comp = await host(c);
  const decision = oneOf(body(c), "decision", DecisionValues);
  const row = await answering(c, comp, id!);
  if (status(row, c.now) === "expired") throw new ApiError("requestExpired");
  if (decision === "deny") {
    const r = await c.env.DB.prepare("UPDATE peer_requests SET status = 'denied', decided_at = ? WHERE id = ? AND status = 'pending'")
      .bind(c.now, id)
      .run();
    if (r.meta.changes === 0 && row.status !== "denied") throw new ApiError("conflict");
    await audit(c.env, `computer:${comp.id}`, "peer.deny", id!, "ok", c.now).run();
    return { status: "denied" };
  }
  const g = randomId("pgr_");
  // Both computers must still have the same owner at the moment of approval. A pairing is new each time: the earlier
  // grant between the two (another key) ends with it.
  const [approved] = await c.env.DB.batch([
    c.env.DB.prepare(
      `UPDATE peer_requests SET status = 'approved', decided_at = ?1, grant_id = ?2
       WHERE id = ?3 AND to_computer_id = ?4 AND status = 'pending' AND expires_at > ?1 AND device_nonce IS NOT NULL
         AND EXISTS(SELECT 1 FROM computers a JOIN computers b ON b.id = ?4
                    WHERE a.id = peer_requests.from_computer_id AND a.status = 'active' AND b.status = 'active'
                      AND a.owner_user_id IS NOT NULL AND a.owner_user_id = b.owner_user_id)`,
    ).bind(c.now, g, id, comp.id),
    c.env.DB.prepare(
      `UPDATE peer_grants SET status = 'revoked', revoked_at = ?1, revoked_reason = 'repaired'
       WHERE status = 'active' AND to_computer_id = ?2
         AND from_computer_id = (SELECT from_computer_id FROM peer_requests WHERE id = ?3 AND grant_id = ?4)`,
    ).bind(c.now, comp.id, id, g),
    c.env.DB.prepare(
      `INSERT INTO peer_grants(id, from_computer_id, to_computer_id, device_key, status, created_at)
       SELECT ?1, from_computer_id, to_computer_id, device_key, 'active', ?2 FROM peer_requests WHERE id = ?3 AND grant_id = ?1`,
    ).bind(g, c.now, id),
  ]);
  if (approved!.meta.changes === 0) {
    const latest = await answering(c, comp, id!);
    if (latest.status === "approved") return { status: "approved", grantId: latest.grant_id };
    if (status(latest, c.now) === "expired") throw new ApiError("requestExpired");
    throw new ApiError("conflict", latest.device_nonce ? "The request can't be approved" : "The other computer hasn't revealed its code yet");
  }
  await audit(c.env, `computer:${comp.id}`, "peer.approve", id!, g, c.now).run();
  await notify(c.env, row.from_computer_id);
  return { status: "approved", grantId: g };
}

/** Either side ends a pairing. */
export async function hostRevokePeer(c: Ctx, [grantId]: string[]): Promise<Responses["hostRevokePeer"]> {
  const comp = await host(c);
  const g = await c.env.DB.prepare("SELECT * FROM peer_grants WHERE id = ? AND (from_computer_id = ?2 OR to_computer_id = ?2)")
    .bind(grantId, comp.id)
    .first<{ id: string; from_computer_id: string; to_computer_id: string; device_key: string; status: string }>();
  if (!g) throw new ApiError("notFound");
  if (g.status !== "active") return {};
  await c.env.DB.batch([
    c.env.DB.prepare("UPDATE peer_grants SET status = 'revoked', revoked_at = ?, revoked_reason = 'unpaired' WHERE id = ? AND status = 'active'").bind(
      c.now,
      g.id,
    ),
    audit(c.env, `computer:${comp.id}`, "peer.revoke", g.id, "ok", c.now),
  ]);
  await notify(c.env, g.to_computer_id, [{ dk: g.device_key, grantId: g.id }]);
  await notify(c.env, g.from_computer_id);
  return {};
}

/** hostState's peer fields: pending requests to this computer, and the active pairings it is on either side of. */
export async function peerState(c: Ctx, comp: ComputerRow) {
  const requests = await c.env.DB.prepare(
    `SELECT r.*, a.name FROM peer_requests r JOIN computers a ON a.id = r.from_computer_id JOIN computers b ON b.id = r.to_computer_id
     WHERE r.to_computer_id = ? AND r.status = 'pending' AND r.expires_at > ?
       AND a.status = 'active' AND a.owner_user_id IS NOT NULL AND a.owner_user_id = b.owner_user_id
     ORDER BY r.created_at`,
  )
    .bind(comp.id, c.now)
    .all<PeerRequestRow & { name: string }>();
  const grants = await c.env.DB.prepare(
    `SELECT g.id, g.from_computer_id, g.to_computer_id, g.device_key, a.name AS from_name, b.name AS to_name
     FROM peer_grants g JOIN computers a ON a.id = g.from_computer_id JOIN computers b ON b.id = g.to_computer_id
     WHERE g.status = 'active' AND (g.from_computer_id = ?1 OR g.to_computer_id = ?1)
       AND a.status = 'active' AND b.status = 'active' AND a.owner_user_id IS NOT NULL AND a.owner_user_id = b.owner_user_id
     ORDER BY g.created_at`,
  )
    .bind(comp.id)
    .all<{ id: string; from_computer_id: string; to_computer_id: string; device_key: string; from_name: string; to_name: string }>();
  return {
    peerRequests: requests.results.map((r) => ({
      requestId: r.id,
      computerId: r.from_computer_id,
      name: r.name,
      deviceKey: r.device_key,
      commit: r.commit_hash,
      ...(r.host_nonce ? { hostNonce: r.host_nonce } : {}),
      ...(r.device_nonce ? { deviceNonce: r.device_nonce } : {}),
      createdAt: r.created_at,
      expiresAt: r.expires_at,
    })),
    peers: grants.results.map((g) => ({
      grantId: g.id,
      fromComputerId: g.from_computer_id,
      toComputerId: g.to_computer_id,
      name: g.from_computer_id === comp.id ? g.to_name : g.from_name,
      deviceKey: g.device_key,
    })),
  };
}
