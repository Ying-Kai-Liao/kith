// Kith push relay.
//
// The APNs key lives only here. The phone registers its device (or Live
// Activity) token and gets back an opaque, encrypted *ticket*; it hands the
// ticket to its own kith host, which can then ask the relay to push to that
// one device — without ever learning the raw token or holding a shared secret.
//
// A ticket names the one computer it was registered for and expires. The computer proves who it is by
// signing its push with its identity key (Kith-Sig, the same header the cloud checks); the relay derives
// the computer id from that key. Tickets minted before this (no computer, no expiry) and unsigned pushes
// from hosts that predate it keep working until LEGACY_TICKETS_UNTIL, so phones and hosts can update.
//
//   POST /register { token, env: "sandbox" | "production", kind: "alert" | "liveactivity", computerId } -> { ticket }
//   POST /push     { ticket, alert?: { title, body }, threadId?, category?, data?, mutableContent?,
//                    liveActivity?: { event: "update" | "end", contentState } }   (Kith-Sig header)
//
// `mutableContent: true` sets `mutable-content: 1` so the Notification Service Extension can replace the
// generic alert with the host's end-to-end encrypted title/body (`data.sealed`).

import { ApnsClient, Notification, PushType, Priority } from "@fivesheepco/cloudflare-apns2";
import { FcmError, fcmClient } from "./fcm.ts";

export interface Env {
  APNS_TEAM_ID: string;
  APNS_KEY_ID: string;
  APNS_SIGNING_KEY: string;
  /** base64 of 32 random bytes: `openssl rand -base64 32` */
  TICKET_KEY: string;
  /** JSON service account with Firebase Cloud Messaging API Admin permission. */
  FCM_SERVICE_ACCOUNT?: string;
  /** ISO date (wrangler.toml `[vars]`). Until then, tickets without a computer or expiry and unsigned pushes are
   * accepted, and `/register` without `computerId` mints such a ticket. Unset or invalid = already past. */
  LEGACY_TICKETS_UNTIL?: string;
}

type ApnsEnv = "sandbox" | "production";
type Kind = "alert" | "liveactivity";
/** `h`: the computer allowed to push with it; `x`: expiry, Unix seconds. A ticket without `x` is a legacy ticket. */
interface ApnsTicket { p?: "apns"; t: string; e: ApnsEnv; k: Kind; b?: string; h?: string; x?: number }
interface FcmTicket { p: "fcm"; t: string; k: Kind; c: string; h: string; b?: string; r?: string; x?: number }
type TicketPayload = ApnsTicket | FcmTicket;

const BUNDLE_ID = "com.yingkailiao.kith.ios";
const COMPUTER_ID = /^[A-Za-z0-9_-]{22}$/;
/** Phones mint a fresh alert ticket every time they connect to the computer; activities live hours, not days. */
export const TICKET_TTL: Record<Kind, number> = { alert: 90 * 86400, liveactivity: 7 * 86400 };

const legacyOpen = (env: Env, now: number) => now < (Date.parse(env.LEGACY_TICKETS_UNTIL ?? "") || 0);

// ---- tickets (AES-GCM, key from TICKET_KEY) ----

const b64url = (bytes: Uint8Array) =>
  btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

const fromB64url = (s: string) => {
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4);
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
};

let cachedKey: { raw: string; key: CryptoKey } | null = null;

async function ticketKey(env: Env): Promise<CryptoKey> {
  if (cachedKey?.raw === env.TICKET_KEY) return cachedKey.key;
  const raw = Uint8Array.from(atob(env.TICKET_KEY), (c) => c.charCodeAt(0));
  const key = await crypto.subtle.importKey("raw", raw, "AES-GCM", false, ["encrypt", "decrypt"]);
  cachedKey = { raw: env.TICKET_KEY, key };
  return key;
}

export async function sealTicket(env: Env, p: TicketPayload): Promise<string> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const key = await ticketKey(env);
  const ct = new Uint8Array(
    await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, new TextEncoder().encode(JSON.stringify(p))),
  );
  const out = new Uint8Array(iv.length + ct.length);
  out.set(iv);
  out.set(ct, iv.length);
  return b64url(out);
}

export async function openTicket(env: Env, ticket: string): Promise<TicketPayload | null> {
  try {
    const bytes = fromB64url(ticket);
    const key = await ticketKey(env);
    const pt = await crypto.subtle.decrypt({ name: "AES-GCM", iv: bytes.slice(0, 12) }, key, bytes.slice(12));
    return JSON.parse(new TextDecoder().decode(pt)) as TicketPayload;
  } catch {
    return null;
  }
}

// ---- APNs clients, cached per isolate (JWT reused while valid) ----

const clients = new Map<string, ApnsClient>();

export function apnsTopic(bundleId: string | undefined, kind: Kind): string {
  const bundle = bundleId ?? BUNDLE_ID;
  if (bundle !== BUNDLE_ID && bundle !== `${BUNDLE_ID}.dev`) throw new Error("invalid bundle identifier");
  return kind === "liveactivity" ? `${bundle}.push-type.liveactivity` : bundle;
}

function client(env: Env, apnsEnv: ApnsEnv, kind: Kind, bundleId?: string): ApnsClient {
  const topic = apnsTopic(bundleId, kind);
  const id = `${env.APNS_KEY_ID}:${apnsEnv}:${topic}`;
  let c = clients.get(id);
  if (!c) {
    c = new ApnsClient({
      team: env.APNS_TEAM_ID,
      keyId: env.APNS_KEY_ID,
      signingKey: env.APNS_SIGNING_KEY.replace(/\\n/g, "\n"),
      defaultTopic: topic,
      host: apnsEnv === "production" ? "api.push.apple.com" : "api.sandbox.push.apple.com",
    });
    clients.set(id, c);
  }
  return c;
}

const json = (body: unknown, status = 200) => Response.json(body, { status });

async function register(req: Request, env: Env, now = Date.now()): Promise<Response> {
  const expires = (k: Kind) => Math.floor(now / 1000) + TICKET_TTL[k];
  const parsed = await req.json().catch(() => null);
  const body = parsed as { provider?: string; token?: string; env?: string; kind?: string; ctx?: string; computerId?: string; botId?: string; taskId?: string; bundleId?: string } | null;
  if (body?.provider === "fcm") {
    if (typeof body.token !== "string" || !/^[A-Za-z0-9_:.-]{20,4096}$/.test(body.token)) return json({ error: "invalid token" }, 400);
    if (body.kind !== "alert" && body.kind !== "liveactivity") return json({ error: "invalid kind" }, 400);
    if (typeof body.ctx !== "string" || !/^(local|[a-f0-9]{64})$/.test(body.ctx) ||
        typeof body.computerId !== "string" || !COMPUTER_ID.test(body.computerId) ||
        (body.kind === "liveactivity" && (typeof body.botId !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(body.botId) ||
          typeof body.taskId !== "string" || !/^[a-f0-9-]{36}$/.test(body.taskId)))) {
      return json({ error: "invalid context" }, 400);
    }
    const ticket = await sealTicket(env, { p: "fcm", t: body.token, k: body.kind, c: body.ctx, h: body.computerId,
      ...(body.kind === "liveactivity" ? { b: body.botId, r: body.taskId } : {}), x: expires(body.kind) });
    return json({ ticket });
  }
  if (body?.provider !== undefined && body.provider !== "apns") return json({ error: "invalid provider" }, 400);
  if (typeof body?.token !== "string" || !/^(?:[0-9a-fA-F]{2}){16,100}$/.test(body.token)) return json({ error: "invalid token" }, 400);
  if (body.env !== "production" && body.env !== "sandbox") return json({ error: "invalid environment" }, 400);
  if (body.kind !== "alert" && body.kind !== "liveactivity") return json({ error: "invalid kind" }, 400);
  if (body.bundleId !== undefined && body.bundleId !== BUNDLE_ID && body.bundleId !== `${BUNDLE_ID}.dev`) return json({ error: "invalid bundle identifier" }, 400);
  // Apps before computer-bound tickets don't send `computerId`; they get a legacy ticket while that's allowed.
  if (body.computerId === undefined ? !legacyOpen(env, now) : typeof body.computerId !== "string" || !COMPUTER_ID.test(body.computerId)) {
    return json({ error: "invalid computerId" }, 400);
  }
  const e: ApnsEnv = body.env;
  const k: Kind = body.kind === "liveactivity" ? "liveactivity" : "alert";
  return json({ ticket: await sealTicket(env, { t: body.token, e, k, ...(body.bundleId ? { b: body.bundleId } : {}),
    ...(body.computerId ? { h: body.computerId, x: expires(k) } : {}) }) });
}

export interface PushBody {
  ticket?: string;
  alert?: { title?: string; body?: string };
  threadId?: string;
  category?: string;
  data?: Record<string, unknown>;
  mutableContent?: boolean;
  liveActivity?: { event?: "update" | "end"; timestamp?: number; staleDate?: number; contentState?: Record<string, unknown> };
}

/** The `aps` dictionary for an alert push. */
export function alertAps(body: PushBody & { alert: NonNullable<PushBody["alert"]> }): Record<string, unknown> {
  const aps: Record<string, unknown> = {
    alert: { title: (body.alert.title ?? "Kith").slice(0, 120), body: (body.alert.body ?? "").slice(0, 400) },
    sound: "default",
    "thread-id": body.threadId,
    category: body.category,
  };
  if (body.mutableContent === true) aps["mutable-content"] = 1;
  return aps;
}

/** ActivityKit dates use Unix seconds; startedAt inside content-state uses Swift's Date epoch. */
export function liveActivityAps(la: NonNullable<PushBody["liveActivity"]>, now = Math.floor(Date.now() / 1000)): Record<string, unknown> {
  const state = la.contentState;
  if (!state || typeof state.status !== "string" || !["working", "needsInput", "idle", "error"].includes(state.status) ||
      state.activity !== "" || !(state.startedAt == null || (typeof state.startedAt === "number" && Number.isFinite(state.startedAt)))) {
    throw new Error("invalid contentState");
  }
  if (la.event !== "update" && la.event !== "end") throw new Error("invalid activity event");
  const timestamp = la.timestamp ?? now;
  const staleDate = la.staleDate ?? timestamp + 900;
  if (!Number.isSafeInteger(timestamp) || timestamp < 0 || timestamp > now + 60 ||
      !Number.isSafeInteger(staleDate) || staleDate < timestamp) throw new Error("invalid activity dates");
  const aps: Record<string, unknown> = {
    timestamp, event: la.event,
    "content-state": { status: state.status, activity: "", startedAt: state.startedAt ?? null },
  };
  if (la.event === "end") aps["dismissal-date"] = timestamp + 60;
  else aps["stale-date"] = staleDate;
  return aps;
}

export function tokenIsGone(error: { reason?: string; statusCode?: number }): boolean {
  return error.statusCode === 410 || ["Unregistered", "BadDeviceToken"].includes(error.reason ?? "");
}

// ---- who is pushing (Kith-Sig, as in serve/cloud/src/auth.ts, "kith" label family only) ----

/** `computerId` is set when the request was signed by that computer's identity key. */
export interface Pusher { computerId?: string }

const SIG_WINDOW_MS = 300_000;
const sha256 = async (bytes: Uint8Array) => new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));

/** Strict base64url of exactly `len` bytes, or null. */
function b64urlBytes(s: string | undefined, len: number): Uint8Array | null {
  if (s === undefined || !/^[A-Za-z0-9_-]*$/.test(s)) return null;
  try {
    const bytes = fromB64url(s);
    return bytes.length === len && b64url(bytes) === s ? bytes : null;
  } catch {
    return null;
  }
}

/** `{}` for an unsigned request (a host from before signed pushes), null for a signature that doesn't verify.
 * Replay inside the 5-minute window is not tracked: it needs the TLS traffic, and the rate limit still applies. */
export async function pusherOf(req: Request, body: Uint8Array, now = Date.now()): Promise<Pusher | null> {
  const header = req.headers.get("Kith-Sig");
  if (!header) return {};
  const f = new Map<string, string>();
  for (const part of header.split(",")) {
    const i = part.indexOf("=");
    if (i <= 0) return null;
    f.set(part.slice(0, i).trim(), part.slice(i + 1).trim());
  }
  const pub = b64urlBytes(f.get("kid"), 32);
  const sig = b64urlBytes(f.get("sig"), 64);
  const nonce = f.get("nonce") ?? "";
  const ts = f.get("ts") ?? "";
  if (f.get("v") !== "1" || !pub || !sig || !b64urlBytes(nonce, 16) || !/^\d{1,16}$/.test(ts) ||
      Math.abs(now - Number(ts)) > SIG_WINDOW_MS) return null;
  const url = new URL(req.url);
  const signed = ["kith-sig-v1", req.method.toUpperCase(), url.host.toLowerCase(), url.pathname + url.search, ts, nonce,
    b64url(await sha256(body))].join("\n");
  try {
    const key = await crypto.subtle.importKey("raw", pub, { name: "Ed25519" }, false, ["verify"]);
    if (!await crypto.subtle.verify({ name: "Ed25519" }, key, sig, new TextEncoder().encode(signed))) return null;
  } catch {
    return null;
  }
  return { computerId: b64url((await sha256(pub)).subarray(0, 16)) };
}

/** Why this pusher may not use this ticket now, or null. `gone: true` tells the host to forget the ticket: it can never
 * work for this host. A refusal that is the request's fault (unsigned, bad signature) leaves it out, so no ticket is lost. */
export function refusal(t: TicketPayload, who: Pusher, body: PushBody, env: Env, now = Date.now()): Response | null {
  const legacy = legacyOpen(env, now);
  if (t.x === undefined ? !legacy : t.x * 1000 <= now) return json({ error: "ticket expired", gone: true }, 410);
  if (who.computerId === undefined && !legacy) return json({ error: "push must be signed" }, 403);
  // Hosts from before signed pushes still say which computer they are in an alert's data.
  const claimed = who.computerId ?? body.data?.computerId;
  if (t.h !== undefined && claimed !== undefined && claimed !== t.h) return json({ error: "ticket belongs to another computer", gone: true }, 403);
  return null;
}

// ---- rate limit: per device, in memory per isolate ----

/** Pushes one device may receive per window. Counted per Worker isolate, so a sender spread over many
 * Cloudflare locations gets this per location; the host itself sends far fewer (5 s per bot and kind). */
export const RATE = { limit: 60, windowMs: 60_000 };
const windows = new Map<string, { start: number; count: number }>();

export function rateLimited(key: string, now = Date.now()): boolean {
  let w = windows.get(key);
  if (!w || now - w.start >= RATE.windowMs) {
    if (windows.size >= 10_000) for (const [k, v] of windows) if (now - v.start >= RATE.windowMs) windows.delete(k);
    w = { start: now, count: 0 };
    windows.set(key, w);
  }
  return ++w.count > RATE.limit;
}

async function readBody(req: Request): Promise<{ raw: Uint8Array; parsed: unknown }> {
  const raw = new Uint8Array(await req.arrayBuffer().catch(() => new ArrayBuffer(0)));
  let parsed: unknown = null;
  try { parsed = JSON.parse(new TextDecoder().decode(raw)); } catch { /* invalid JSON */ }
  return { raw, parsed };
}

async function push(req: Request, env: Env): Promise<Response> {
  const { raw, parsed } = await readBody(req);
  const who = await pusherOf(req, raw);
  if (!who) return json({ error: "invalid signature" }, 403);
  return pushOne(parsed as PushBody | null, who, env);
}

export async function pushOne(body: PushBody | null, who: Pusher, env: Env, now = Date.now()): Promise<Response> {
  const t = typeof body?.ticket === "string" ? await openTicket(env, body.ticket) : null;
  if (!body || !t) return json({ error: "invalid ticket", gone: true }, 403);
  const refused = refusal(t, who, body, env, now);
  if (refused) return refused;
  if (rateLimited(`${t.p ?? "apns"}:${t.t}`, now)) return json({ error: "too many pushes" }, 429);
  try {
    if (t.p === "fcm") {
      const data = fcmData(t, body);
      const urgent = t.k === "alert" || body.liveActivity?.event === "end" || body.liveActivity?.contentState?.status === "needsInput";
      const sender = fcmClient(env.FCM_SERVICE_ACCOUNT);
      await sender.push(t.t, data, urgent);
      return json({ ok: true });
    }
    if (t.k === "alert") {
      if (!body.alert || typeof body.alert !== "object" ||
          (body.alert.title !== undefined && typeof body.alert.title !== "string") ||
          (body.alert.body !== undefined && typeof body.alert.body !== "string") ||
          (body.data !== undefined && (!body.data || typeof body.data !== "object" || Array.isArray(body.data) || "aps" in body.data))) {
        return json({ error: "invalid alert" }, 400);
      }
      const notification = new Notification(t.t, {
          type: PushType.alert,
          priority: Priority.immediate,
          aps: alertAps({ ...body, alert: body.alert }),
          data: body.data ?? {},
          expiration: Math.floor(Date.now() / 1000) + 3600,
        });
      if (new TextEncoder().encode(JSON.stringify(notification.buildApnsOptions())).length > 4096) {
        return json({ error: "payload too large" }, 413);
      }
      await client(env, t.e, "alert", t.b).send(notification);
    } else {
      const la = body.liveActivity;
      if (!la) return json({ error: "liveActivity required" }, 400);
      let aps: Record<string, unknown>;
      try { aps = liveActivityAps(la); }
      catch { return json({ error: "invalid liveActivity" }, 400); }
      await client(env, t.e, "liveactivity", t.b).send(
        new Notification(t.t, {
          type: PushType.liveactivity,
          priority: la.event === "end" || la.contentState?.status === "needsInput" ? Priority.immediate : Priority.throttled,
          expiration: Number(aps["stale-date"] ?? aps["dismissal-date"]), aps,
        }),
      );
    }
    return json({ ok: true });
  } catch (err) {
    if (err instanceof FcmError) return json({ error: err.reason, gone: err.gone }, err.gone ? 410 : err.reason === "payload too large" ? 413 : 502);
    if (err instanceof InvalidFcmPayload) return json({ error: "invalid FCM payload" }, 400);
    // ApnsError carries APNs' `reason` (e.g. "Unregistered") and `statusCode`.
    const e = err as { reason?: string; statusCode?: number; message?: string };
    const reason = e.reason ?? e.message ?? String(err);
    // Dead device token: tell the host to drop the ticket.
    const gone = tokenIsGone(e);
    return json({ error: reason, gone }, gone ? 410 : 502);
  }
}

/** One physical APNs destination receives only the newest registration for this event.
 * The host cannot do this comparison itself because it never sees raw APNs tokens.
 */
export function latestTicketIndices(tickets: Array<TicketPayload | null>): Set<number> {
  const latest = new Map<string, number>();
  tickets.forEach((ticket, index) => {
    if (ticket?.p === "fcm") latest.set(`fcm:${ticket.k}:${ticket.b ?? ""}:${ticket.t}`, index);
    else if (ticket) latest.set(`apns:${ticket.b ?? BUNDLE_ID}:${ticket.e}:${ticket.k}:${ticket.t.toLowerCase()}`, index);
  });
  return new Set(latest.values());
}

class InvalidFcmPayload extends Error {}

/** Data-only messages let Android open sealed text locally even in the background. */
export function fcmData(ticket: FcmTicket, body: PushBody): Record<string, string> {
  if (ticket.k === "alert") {
    const data = body.data;
    if (!body.alert || typeof body.alert.title !== "string" || typeof body.alert.body !== "string" ||
        !data || typeof data.sealed !== "string" || typeof data.botId !== "string" ||
        data.ctx !== ticket.c || data.computerId !== ticket.h) throw new InvalidFcmPayload();
    return { kind: "alert", ctx: ticket.c, computerId: ticket.h, botId: data.botId, sealed: data.sealed,
      title: body.alert.title.slice(0, 120), body: body.alert.body.slice(0, 400), category: body.category ?? "" };
  }
  if (!body.liveActivity || !ticket.b || !ticket.r) throw new InvalidFcmPayload();
  let aps: Record<string, unknown>;
  try { aps = liveActivityAps(body.liveActivity); } catch { throw new InvalidFcmPayload(); }
  return { kind: "task", ctx: ticket.c, computerId: ticket.h, botId: ticket.b, taskId: ticket.r,
    event: String(aps.event), timestamp: String(aps.timestamp),
    staleDate: String(aps["stale-date"] ?? aps["dismissal-date"]), status: String(body.liveActivity.contentState?.status) };
}

export async function pushBatch(req: Request, env: Env, deliver = pushOne): Promise<Response> {
  const { raw, parsed } = await readBody(req);
  const who = await pusherOf(req, raw);
  if (!who) return json({ error: "invalid signature" }, 403);
  const body = parsed as { notifications?: PushBody[] } | null;
  const notifications = body?.notifications;
  if (!Array.isArray(notifications) || notifications.length === 0 || notifications.length > 256) {
    return json({ error: "invalid notification batch" }, 400);
  }
  const tickets = await Promise.all(notifications.map(notification =>
    typeof notification?.ticket === "string" ? openTicket(env, notification.ticket) : null));
  const latest = latestTicketIndices(tickets);
  const results = await Promise.all(notifications.map(async (notification, index) => {
    if (!tickets[index]) return { index, status: 403, gone: true };
    if (!latest.has(index)) return { index, status: 200, superseded: true };
    const response = await deliver(notification, who, env);
    const reply = await response.clone().json().catch(() => null) as { gone?: unknown } | null;
    return reply?.gone === true ? { index, status: response.status, gone: true } : { index, status: response.status };
  }));
  return json({ results });
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(req.url);
    if (req.method === "GET" && pathname === "/health") return json({ ok: true });
    if (req.method !== "POST") return json({ error: "method not allowed" }, 405);
    if (pathname === "/register") return register(req, env);
    if (pathname === "/push") return push(req, env);
    if (pathname === "/push-batch") return pushBatch(req, env);
    return json({ error: "not found" }, 404);
  },
};
