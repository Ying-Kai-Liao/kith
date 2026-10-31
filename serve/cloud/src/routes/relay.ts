// Relay WebSocket upgrades (§7.1) and public routine webhook deliveries (§7.8), both forwarded
// to the computer's Durable Object.

import { COMPUTER_ID, computerIdFor, fromB64url, verifyClerkToken, verifySig, type SignedRequest } from "../auth";
import { isErrorCode, type ErrorCode } from "../contract.gen";
import { HOOK_ID, HOOK_MAX_BODY } from "../hooks";
import { ApiError, webSessionLive, type Ctx } from "./common";

function checkUpgrade(c: Ctx): void {
  if (c.url.searchParams.get("v") !== "1") throw new ApiError("upgradeRequired");
  if (c.req.headers.get("Upgrade")?.toLowerCase() !== "websocket") throw new ApiError("badRequest", "Expected a WebSocket upgrade");
}

/** Forwards the upgrade to the computer's DO in a request the Worker builds itself; the client's URL never reaches it. */
async function forward(
  c: Ctx,
  computerId: string,
  role: "host" | "device",
  s: SignedRequest,
  pair: string,
  web?: { sid: string | null },
): Promise<Response> {
  const headers = new Headers({
    "X-Kith-Internal": "1",
    "X-Kith-Role": role,
    "X-Kith-Key": s.kid,
    "X-Kith-Nonce": s.nonce,
    "X-Kith-Ts": String(s.ts),
    "X-Kith-Pair": pair,
    "X-Kith-Computer": computerId,
  });
  for (const [name, value] of c.req.headers) {
    // A browser's subprotocol list carries its session JWT: the DO gets the facts, never the token.
    if (name === "upgrade" || (name.startsWith("sec-websocket-") && !web)) headers.set(name, value);
  }
  if (web) {
    headers.set("X-Kith-Subproto", "1");
    headers.set("X-Kith-Origin", c.req.headers.get("Origin") ?? "");
    if (web.sid) headers.set("X-Kith-Web", web.sid);
  }
  const res = await c.env.RELAY.get(c.env.RELAY.idFromName(computerId)).fetch(new Request("https://do/relay", { headers }));
  if (res.status === 101) return res;
  const err = (await res.json().catch(() => null)) as { error?: { code?: unknown } } | null;
  const code = err?.error?.code;
  throw new ApiError(isErrorCode(code) ? code : "internal", undefined, res.status);
}

export async function relayHost(c: Ctx): Promise<Response> {
  checkUpgrade(c);
  const s = await verifySig(c.req, c.raw, c.now);
  if (!s) throw new ApiError("badSignature");
  const id = await computerIdFor(fromB64url(s.kid, 32)!);
  const row = await c.env.DB.prepare("SELECT status FROM computers WHERE id = ?").bind(id).first<{ status: string }>();
  if (!row) throw new ApiError("unknownComputer", "Register first (POST /v1/host/register)");
  if (row.status !== "active") throw new ApiError("forbidden");
  return forward(c, id, "host", s, "");
}

/** The browser form of the upgrade (design 4.6): no Kith-Sig header, the credentials ride in `Sec-WebSocket-Protocol`. */
function subprotocols(c: Ctx): { sig: string; jwt: string | null } | null {
  if (c.req.headers.has("Kith-Sig")) return null;
  const offered = (c.req.headers.get("Sec-WebSocket-Protocol") ?? "").split(",").map((p) => p.trim());
  if (!offered.includes("kith.v1")) return null;
  const sig = offered.find((p) => p.startsWith("kith-sig."))?.slice("kith-sig.".length);
  const bytes = sig ? fromB64url(sig) : null;
  const jwt = offered.find((p) => p.startsWith("kith-jwt."))?.slice("kith-jwt.".length) ?? null;
  return { sig: bytes ? new TextDecoder().decode(bytes) : "", jwt };
}

const CLOSE_FOR: Partial<Record<ErrorCode, number>> = {
  unknownComputer: 4404,
  upgradeRequired: 4426,
  unauthenticated: 4401,
  badSignature: 4401,
  forbidden: 4403,
  rateLimited: 4008,
  badRequest: 4400,
};

/** A browser can't read an HTTP status off a refused upgrade, so it is accepted and closed with a code instead. */
function refusal(code: ErrorCode): Response {
  const { 0: client, 1: server } = new WebSocketPair();
  server.accept();
  server.close(CLOSE_FOR[code] ?? 1011, code);
  return new Response(null, { status: 101, webSocket: client, headers: { "Sec-WebSocket-Protocol": "kith.v1" } });
}

export async function relayDevice(c: Ctx, [computerId]: string[]): Promise<Response> {
  const sub = c.req.headers.get("Upgrade")?.toLowerCase() === "websocket" ? subprotocols(c) : null;
  if (!sub) return admitDevice(c, computerId!, null);
  try {
    return await admitDevice(c, computerId!, sub);
  } catch (e) {
    if (e instanceof ApiError) return refusal(e.code);
    throw e;
  }
}

async function admitDevice(c: Ctx, computerId: string, sub: { sig: string; jwt: string | null } | null): Promise<Response> {
  checkUpgrade(c);
  if (!COMPUTER_ID.test(computerId)) throw new ApiError("badRequest", "Invalid computerId");
  const pair = c.url.searchParams.get("pair") ?? "";
  if (pair && !fromB64url(pair, 16)) throw new ApiError("badRequest", "Invalid pair");
  const s = await verifySig(c.req, c.raw, c.now, sub ? sub.sig : undefined);
  if (!s) throw new ApiError("badSignature");
  // Only registered computers get a DO; random IDs never create one.
  const row = await c.env.DB.prepare("SELECT status FROM computers WHERE id = ?").bind(computerId).first<{ status: string }>();
  if (!row || row.status !== "active") throw new ApiError("unknownComputer");
  // A web key is admitted only with a live login (design 4.5). Without a JWT it is refused, so a key that registered as
  // web can't skip the session checks by connecting like a native device.
  // Every web row for the key, revoked ones too: a revoked or retired web key must never fall through to the native path
  // (no JWT, no Origin, no max-age) just because the DO's block notice failed.
  if (await c.env.DB.prepare("SELECT 1 FROM retired_keys WHERE sign_pub = ?").bind(s.kid).first()) throw new ApiError("forbidden");
  const allWeb = await c.env.DB.prepare("SELECT owner_user_id, clerk_sid, revoked_at FROM devices WHERE sign_pub = ? AND platform = 'web'")
    .bind(s.kid)
    .all<{ owner_user_id: string; clerk_sid: string | null; revoked_at: number | null }>();
  const webRows = { results: allWeb.results.filter((r) => r.revoked_at === null) };
  if (allWeb.results.length && !webRows.results.length) throw new ApiError("forbidden");
  if (!webRows.results.length) {
    if (sub?.jwt) throw new ApiError("forbidden");
    return forward(c, computerId, "device", s, pair, sub ? { sid: null } : undefined);
  }
  if (!sub?.jwt) throw new ApiError("unauthenticated");
  const clerk = await verifyClerkToken(sub.jwt, c.env);
  if (!clerk) throw new ApiError("unauthenticated");
  const bound = webRows.results.find((r) => r.owner_user_id === clerk.userId);
  if (!bound) throw new ApiError("forbidden");
  if (!bound.clerk_sid || bound.clerk_sid !== clerk.sid) throw new ApiError("unauthenticated");
  if (!(await webSessionLive(c.env, clerk))) throw new ApiError("unauthenticated");
  return forward(c, computerId, "device", s, pair, { sid: clerk.sid });
}

// ---- routine webhooks (§7.8) ----

/**
 * `POST /v1/hooks/:computerId/:hookId`: a public delivery for one of the computer's routines. The
 * computer's DO checks the key and queues it for the host; the host checks again before it runs.
 */
export async function routineHook(c: Ctx, [computerId, hookId]: string[]): Promise<Response> {
  if (!COMPUTER_ID.test(computerId!) || !HOOK_ID.test(hookId!)) throw new ApiError("notFound");
  if (c.raw.length > HOOK_MAX_BODY) throw new ApiError("tooLarge", "Deliveries are limited to 64 KB");
  if (c.env.HOOK_LIMITER) {
    const ip = c.req.headers.get("CF-Connecting-IP") ?? "unknown";
    if (!(await c.env.HOOK_LIMITER.limit({ key: `${ip}|${computerId}/${hookId}` })).success) throw new ApiError("rateLimited");
  }
  // Only registered computers get a DO; random IDs never create one.
  const row = await c.env.DB.prepare("SELECT status FROM computers WHERE id = ?").bind(computerId).first<{ status: string }>();
  if (!row || row.status !== "active") throw new ApiError("notFound");
  const headers = new Headers(c.req.headers);
  headers.set("X-Kith-Internal", "1");
  headers.set("X-Kith-Hook", hookId!);
  return c.env.RELAY.get(c.env.RELAY.idFromName(computerId!)).fetch(
    new Request("https://do/internal/hook", { method: "POST", headers, body: c.raw }),
  );
}
