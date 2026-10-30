// /v1 HTTP handlers (spec §8.4) and the relay upgrade routes (§7.1). Route groups live in
// `routes/`; this module is what the router and the cron call.

import type { Env } from "./index";
import type { Ctx } from "./routes/common";
import type { Responses } from "./contract.gen";

export { ApiError, type Ctx } from "./routes/common";
export * from "./routes/account";
export * from "./routes/botguests";
export * from "./routes/host";
export * from "./routes/invites";
export * from "./routes/peers";
export * from "./routes/relay";
export * from "./routes/webhooks";

export const VERSION = "0.1.0";

// ---- public ----

export const health = async (): Promise<Responses["health"]> => ({ ok: true, version: VERSION });

/**
 * Where a connector's sign-in page returns when the user signed in on the phone. Stateless: it bounces
 * the query (code, state or error) to the app, which hands it to the host over the E2E channel. The code
 * is useless without the PKCE verifier that only the host holds.
 */
export const oauthKithCallback = async (c: Ctx) =>
  new Response(null, { status: 302, headers: { Location: `kith://oauth${c.url.search}`, "Cache-Control": "no-store" } });

export const oauthKithDevCallback = async (c: Ctx) =>
  new Response(null, { status: 302, headers: { Location: `kith-dev://oauth${c.url.search}`, "Cache-Control": "no-store" } });

// ---- cron (every 15 minutes) ----

// Finished records and webhook ids are kept 30 days. Clerk (Svix) retries a failed delivery for about a day, so a
// processed event id is long past any retry by then. Deleted in slices of PRUNE_BATCH so a first sweep over a big
// backlog stays inside the Worker's limits; the cron (every 15 minutes, already there) drains the rest.
const RECORD_KEEP_MS = 30 * 86_400_000;
const PRUNE_BATCH = 500;

export async function sweep(env: Env, now: number): Promise<void> {
  const old = now - RECORD_KEEP_MS;
  await env.DB.batch([
    env.DB.prepare(
      "DELETE FROM processed_events WHERE rowid IN (SELECT rowid FROM processed_events WHERE processed_at <= ? LIMIT ?)",
    ).bind(old, PRUNE_BATCH),
    // Anything not pending (approved, denied, expired, cancelled) is finished; pending ones are expired first.
    env.DB.prepare(
      "DELETE FROM access_requests WHERE id IN (SELECT id FROM access_requests WHERE status != 'pending' AND created_at <= ? LIMIT ?)",
    ).bind(old, PRUNE_BATCH),
    env.DB.prepare("DELETE FROM sig_nonces WHERE expires_at <= ?").bind(now),
    env.DB.prepare("DELETE FROM claims WHERE expires_at <= ?").bind(now - 86_400_000),
    env.DB.prepare(
      `UPDATE access_requests SET status = 'expired', host_nonce = NULL, device_nonce = NULL
       WHERE status = 'pending' AND expires_at <= ?`,
    ).bind(now),
    env.DB.prepare("DELETE FROM audit_events WHERE created_at <= ?").bind(now - 30 * 86_400_000),
    // Unaccepted bot invites only: an accepted one is the record behind a bot device.
    env.DB.prepare("DELETE FROM bot_invites WHERE accepted_at IS NULL AND expires_at <= ?").bind(now - 7 * 86_400_000),
  ]);
}
