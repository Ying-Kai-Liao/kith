// Kith cloud: accounts (Clerk), computers, access grants (D1) and the end-to-end encrypted relay
// (one ComputerRelay Durable Object per computer). The cloud only ever sees ciphertext and routing
// metadata; the host stays the sole authority on which devices may decrypt (docs/reference/remote-relay.md).

import * as api from "./api";
import { webOrigins } from "./auth";
import { ApiError, type Ctx } from "./api";
import { check, ROUTES, type Responses, type RouteName } from "./contract.gen";
import * as legacy from "./legacy-compat";
import * as pages from "./routes/pages";
import * as download from "./routes/download";
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
  /** Tests only: check every JSON answer against the cloud contract (strictProblems). */
  KITH_CONTRACT_STRICT?: string;
}

/**
 * What each route answers with: a JSON route's handler must return its contract type (or a Response carrying it), so a
 * field or value the contract doesn't name is a compile error. Paths, methods and types: serve/contract/cloud/contract.json.
 */
type Handlers = {
  [K in RouteName]: (c: Ctx, params: string[]) => Promise<K extends keyof Responses ? Responses[K] | Response : Response>;
};

/** Above every route's own limit (64 KB bodies); the routes check theirs. */
const MAX_REQUEST = 128 * 1024;

const handlers: Handlers = {
  health: api.health,
  oauthLegacyCallback: legacy.oauthLegacyCallback,
  oauthLegacyDevCallback: legacy.oauthLegacyDevCallback,
  oauthKithCallback: api.oauthKithCallback,
  oauthKithDevCallback: api.oauthKithDevCallback,
  me: api.me,
  registerDevice: api.registerDevice,
  listDevices: api.listDevices,
  retireDevice: api.retireDevice,
  revokeDevice: api.revokeDevice,
  createClaim: api.createClaim,
  completeClaim: api.completeClaim,
  listComputers: api.listComputers,
  renameComputer: api.renameComputer,
  removeComputer: api.removeComputer,
  createAccessRequest: api.createAccessRequest,
  listGrants: api.listGrants,
  revokeGrant: api.revokeGrant,
  getAccessRequest: api.getAccessRequest,
  revealAccessRequest: api.revealAccessRequest,
  cancelAccessRequest: api.cancelAccessRequest,
  hostRegister: api.hostRegister,
  hostState: api.hostState,
  hostScreenIce: api.hostScreenIce,
  hostNonce: api.hostNonce,
  hostDecision: api.hostDecision,
  hostRevokeGrant: api.hostRevokeGrant,
  hostUnclaim: api.hostUnclaim,
  hostCreateInvite: api.hostCreateInvite,
  hostRevokeInvite: api.hostRevokeInvite,
  hostRevokeMembership: api.hostRevokeMembership,
  hostRevokePerson: api.hostRevokePerson,
  hostUnrevokePerson: api.hostUnrevokePerson,
  acceptInvite: api.acceptInvite,
  hostCreateBotInvite: api.hostCreateBotInvite,
  hostRevokeBotInvite: api.hostRevokeBotInvite,
  hostRevokeBotDevice: api.hostRevokeBotDevice,
  acceptBotInvite: api.acceptBotInvite,
  relayHost: api.relayHost,
  // Any segment reaches the handler so a malformed computerId gets its 400 (§7.1).
  relayDevice: api.relayDevice,
  clerkWebhook: api.clerkWebhook,
  routineHook: api.routineHook,
  // The open-in-Kith page behind https://kithbot.app/i/<computer>/<invite>#t=<token>.
  invitePage: pages.invitePage,
  // The desktop download page and its one-click kithbot.app links (302 to the latest GitHub release).
  downloadPage: download.downloadPage,
  downloadRedirect: download.downloadRedirect,
};

const routes = (Object.keys(ROUTES) as RouteName[]).map((name) => ({ name, ...ROUTES[name], handler: handlers[name] }));

/**
 * With KITH_CONTRACT_STRICT (the tests), every JSON answer is checked against its route's contract type as the cloud
 * must send it: a missing key, a value the contract doesn't name or an extra field is a 500 that names it.
 */
async function strictProblems(route: (typeof routes)[number], out: unknown): Promise<string[]> {
  if (!("response" in route)) return [];
  let body = out;
  if (out instanceof Response) {
    if (out.status < 200 || out.status >= 300 || !out.headers.get("Content-Type")?.includes("application/json")) return [];
    body = await out.clone().json();
  }
  return check(route.response, body, "producer").map((p) => `${route.name}: ${p}`);
}

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
    for (const route of routes) {
      const m = route.pattern.exec(url.pathname);
      if (!m || req.method !== route.method) continue;
      // Every route takes small bodies; refuse a big one before reading it.
      if (Number(req.headers.get("content-length") ?? 0) > MAX_REQUEST) throw new ApiError("tooLarge");
      const raw = new Uint8Array(await req.arrayBuffer());
      if (raw.length > MAX_REQUEST) throw new ApiError("tooLarge");
      const out = await route.handler({ req, env, exec, url, raw, now: Date.now() }, m.slice(1));
      if (env.KITH_CONTRACT_STRICT) {
        const problems = await strictProblems(route, out);
        if (problems.length) throw new ApiError("internal", `Not the contract: ${problems.join("; ")}`);
      }
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
