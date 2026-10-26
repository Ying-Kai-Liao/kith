// Kith cloud: accounts (Clerk), computers, access grants (D1) and the end-to-end encrypted relay
// (one ComputerRelay Durable Object per computer). The cloud only ever sees ciphertext and routing
// metadata; the host stays the sole authority on which devices may decrypt (docs/reference/remote-relay.md).

import * as api from "./api";
import { webOrigins } from "./auth";
import { ApiError, type Ctx } from "./api";
import * as legacy from "./legacy-compat";
import * as pages from "./routes/pages";
import type { ComputerRelay } from "./relay";

export { ComputerRelay } from "./relay";

export interface Env {
  DB: D1Database;
  RELAY: DurableObjectNamespace<ComputerRelay>;
  /** Workers Rate Limiting binding for new computer registrations. */
  REGISTER_LIMITER?: RateLimit;
  /** Server-only Cloudflare Realtime TURN key and credential-issuing token. */
  TURN_KEY_ID?: string;
  TURN_KEY_API_TOKEN?: string;
  TURN_LIMITER?: RateLimit;
  /** Public routine webhook deliveries per sender IP and hook. */
  HOOK_LIMITER?: RateLimit;
  /** Bot invite accepts per client IP (POST /v1/bot-invites/:id/accept). */
  BOT_JOIN_LIMITER?: RateLimit;
  CLERK_ISSUER: string;
  CLERK_SECRET_KEY?: string;
  /** dev / e2e only: networkless verification with this public key. */
  CLERK_JWT_KEY?: string;
  CLERK_AUTHORIZED_PARTIES?: string;
  CLERK_WEBHOOK_SECRET?: string;
  /** Comma list of origins that host the web client: the only ones that get CORS, and the only ones a web socket may come from. */
  WEB_ORIGINS?: string;
  /** Seconds a web socket stays open before the DO closes it with 4100 (default 3600). */
  WEB_SOCKET_MAX_AGE?: string;
}

type Handler = (c: Ctx, params: string[]) => Promise<unknown>;

/** Above every route's own limit (64 KB bodies); the routes check theirs. */
const MAX_REQUEST = 128 * 1024;

const ID = "([A-Za-z0-9_-]{1,64})";
const routes: [string, RegExp, Handler][] = [
  ["GET", /^\/v1\/health$/, api.health],
  ["GET", /^\/v1\/oauth\/callback$/, legacy.oauthLegacyCallback],
  ["GET", /^\/v1\/oauth\/callback\/dev$/, legacy.oauthLegacyDevCallback],
  ["GET", /^\/v1\/oauth\/callback\/kith$/, api.oauthKithCallback],
  ["GET", /^\/v1\/oauth\/callback\/kith-dev$/, api.oauthKithDevCallback],
  ["GET", /^\/v1\/me$/, api.me],
  ["POST", /^\/v1\/devices$/, api.registerDevice],
  ["GET", /^\/v1\/devices$/, api.listDevices],
  ["POST", /^\/v1\/devices\/retire$/, api.retireDevice],
  ["DELETE", new RegExp(`^/v1/devices/${ID}$`), api.revokeDevice],
  ["POST", /^\/v1\/claims$/, api.createClaim],
  ["POST", new RegExp(`^/v1/claims/${ID}/complete$`), api.completeClaim],
  ["GET", /^\/v1\/computers$/, api.listComputers],
  ["PATCH", new RegExp(`^/v1/computers/${ID}$`), api.renameComputer],
  ["DELETE", new RegExp(`^/v1/computers/${ID}$`), api.removeComputer],
  ["POST", new RegExp(`^/v1/computers/${ID}/access-requests$`), api.createAccessRequest],
  ["GET", new RegExp(`^/v1/computers/${ID}/grants$`), api.listGrants],
  ["DELETE", new RegExp(`^/v1/computers/${ID}/grants/${ID}$`), api.revokeGrant],
  ["GET", new RegExp(`^/v1/access-requests/${ID}$`), api.getAccessRequest],
  ["POST", new RegExp(`^/v1/access-requests/${ID}/reveal$`), api.revealAccessRequest],
  ["DELETE", new RegExp(`^/v1/access-requests/${ID}$`), api.cancelAccessRequest],
  ["POST", /^\/v1\/host\/register$/, api.hostRegister],
  ["GET", /^\/v1\/host\/state$/, api.hostState],
  ["POST", /^\/v1\/host\/screen-ice$/, api.hostScreenIce],
  ["POST", new RegExp(`^/v1/host/access-requests/${ID}/nonce$`), api.hostNonce],
  ["POST", new RegExp(`^/v1/host/access-requests/${ID}/decision$`), api.hostDecision],
  ["POST", new RegExp(`^/v1/host/grants/${ID}/revoke$`), api.hostRevokeGrant],
  ["POST", /^\/v1\/host\/unclaim$/, api.hostUnclaim],
  ["POST", /^\/v1\/host\/invites$/, api.hostCreateInvite],
  ["POST", new RegExp(`^/v1/host/invites/${ID}/revoke$`), api.hostRevokeInvite],
  ["POST", new RegExp(`^/v1/host/memberships/${ID}/revoke$`), api.hostRevokeMembership],
  ["POST", /^\/v1\/host\/people\/revoke$/, api.hostRevokePerson],
  ["POST", /^\/v1\/host\/people\/unrevoke$/, api.hostUnrevokePerson],
  ["POST", new RegExp(`^/v1/invites/${ID}/accept$`), api.acceptInvite],
  ["POST", /^\/v1\/host\/bot-invites$/, api.hostCreateBotInvite],
  ["POST", new RegExp(`^/v1/host/bot-invites/${ID}/revoke$`), api.hostRevokeBotInvite],
  ["POST", new RegExp(`^/v1/host/bot-devices/${ID}/revoke$`), api.hostRevokeBotDevice],
  ["POST", new RegExp(`^/v1/bot-invites/${ID}/accept$`), api.acceptBotInvite],
  ["GET", /^\/v1\/relay\/host$/, api.relayHost],
  // Any segment reaches the handler so a malformed computerId gets its 400 (§7.1).
  ["GET", /^\/v1\/relay\/device\/([^/]+)$/, api.relayDevice],
  ["POST", /^\/v1\/webhooks\/clerk$/, api.clerkWebhook],
  ["POST", new RegExp(`^/v1/hooks/${ID}/${ID}$`), api.routineHook],
  // The open-in-Kith page behind https://kithbot.app/i/<computer>/<invite>#t=<token>.
  ["GET", new RegExp(`^/i/${ID}/${ID}$`), pages.invitePage],
];

/** Only an origin listed in WEB_ORIGINS gets CORS; never a wildcard, and none at all when the var is unset. */
const allowedOrigin = (req: Request, env: Env): string | null => {
  const origin = req.headers.get("Origin");
  return origin && webOrigins(env).includes(origin) ? origin : null;
};

function withCors(res: Response, origin: string | null): Response {
  // A WebSocket upgrade has no CORS and its response can't be rebuilt.
  if (!origin || res.status === 101) return res;
  const out = new Response(res.body, res);
  out.headers.set("Access-Control-Allow-Origin", origin);
  out.headers.append("Vary", "Origin");
  return out;
}

async function handle(req: Request, env: Env, exec: ExecutionContext): Promise<Response> {
  const requestId = req.headers.get("cf-ray") ?? crypto.randomUUID();
  try {
    const url = new URL(req.url);
    for (const [method, pattern, handler] of routes) {
      const m = pattern.exec(url.pathname);
      if (!m || req.method !== method) continue;
      // Every route takes small bodies; refuse a big one before reading it.
      if (Number(req.headers.get("content-length") ?? 0) > MAX_REQUEST) throw new ApiError("tooLarge");
      const raw = new Uint8Array(await req.arrayBuffer());
      if (raw.length > MAX_REQUEST) throw new ApiError("tooLarge");
      const out = await handler({ req, env, exec, url, raw, now: Date.now() }, m.slice(1));
      return out instanceof Response ? out : Response.json(out);
    }
    throw new ApiError("notFound");
  } catch (e) {
    const err = e instanceof ApiError ? e : new ApiError("internal");
    if (!(e instanceof ApiError)) console.error("unhandled", { requestId, error: e instanceof Error ? e.stack : String(e) });
    return Response.json({ error: { code: err.code, message: err.message }, requestId }, { status: err.status });
  }
}

export default {
  async fetch(req: Request, env: Env, exec: ExecutionContext): Promise<Response> {
    const origin = allowedOrigin(req, env);
    if (req.method === "OPTIONS" && origin && req.headers.has("Access-Control-Request-Method")) {
      return new Response(null, {
        status: 204,
        headers: {
          "Access-Control-Allow-Origin": origin,
          "Access-Control-Allow-Methods": "GET, POST, DELETE, PATCH, OPTIONS",
          "Access-Control-Allow-Headers": "Authorization, Content-Type, Kith-Sig",
          "Access-Control-Max-Age": "600",
          Vary: "Origin",
        },
      });
    }
    return withCors(await handle(req, env, exec), origin);
  },

  async scheduled(_controller: ScheduledController, env: Env): Promise<void> {
    await api.sweep(env, Date.now());
  },
} satisfies ExportedHandler<Env>;
