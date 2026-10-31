// Alert aps: `mutableContent` passes through as `mutable-content: 1`, and only when asked.
import assert from "node:assert/strict";
import worker, { alertAps, liveActivityAps, tokenIsGone, sealTicket, latestTicketIndices, pushBatch } from "../src/index.ts";

const base = { alert: { title: "Kith", body: "Needs you" }, threadId: "b1", category: "needsInput" };
assert.equal(alertAps({ ...base, mutableContent: true })["mutable-content"], 1);
assert.equal("mutable-content" in alertAps(base), false);
assert.equal("mutable-content" in alertAps({ ...base, mutableContent: false }), false);
assert.deepEqual(alertAps(base).alert, { title: "Kith", body: "Needs you" });
console.log("push tests ok");

const state = { status: "working", activity: "", startedAt: 1 };
const update = liveActivityAps({ event: "update", timestamp: 100, contentState: state }, 100);
assert.equal(update.timestamp, 100);
assert.equal(update["stale-date"], 1000);
assert.equal("dismissal-date" in update, false);
const ended = liveActivityAps({ event: "end", timestamp: 100, contentState: { ...state, status: "error" } }, 150);
assert.equal(ended["dismissal-date"], 160);
assert.equal((ended["content-state"] as typeof state).status, "error");
assert.equal("stale-date" in ended, false);
assert.throws(() => liveActivityAps({ event: "update", contentState: { ...state, activity: "private task text" } }));
assert.throws(() => liveActivityAps({ event: "update", timestamp: 200, contentState: state }, 100));
assert.throws(() => liveActivityAps({ event: "update", contentState: { ...state, status: "unknown" } }));
assert.throws(() => liveActivityAps({ event: "update", contentState: { ...state, status: ["working"] } }));
assert.equal(tokenIsGone({ reason: "ExpiredProviderToken", statusCode: 403 }), false);
assert.equal(tokenIsGone({ reason: "Unregistered", statusCode: 410 }), true);
assert.equal(tokenIsGone({ reason: "BadDeviceToken", statusCode: 400 }), true);
console.log("Live Activity payload tests ok");

// Legacy tickets (no computer, no expiry) and unsigned pushes, as before computer-bound tickets.
const env = { TICKET_KEY: btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(32)))), LEGACY_TICKETS_UNTIL: "2999-01-01" } as any;
const ticket = await sealTicket(env, { t: "ab".repeat(32), e: "sandbox", k: "alert" });
async function request(path: string, body: unknown) {
  return worker.fetch(new Request(`https://relay.test${path}`, { method: "POST", body: JSON.stringify(body) }), env);
}
assert.equal((await request("/register", { token: "ab".repeat(32), env: "typo", kind: "alert" })).status, 400);
assert.equal((await request("/push", { ticket, alert: { title: 123 } })).status, 400);
assert.equal((await request("/push", { ticket, alert: { body: "fallback" }, data: { aps: {} } })).status, 400);
assert.equal((await request("/push", { ticket, alert: { body: "fallback" }, data: { sealed: "x".repeat(4096) } })).status, 413);
const activityTicket = await sealTicket(env, { t: "ab".repeat(32), e: "sandbox", k: "liveactivity" });
assert.equal((await request("/push", { ticket: activityTicket, liveActivity: { event: "start", contentState: state } })).status, 400);
console.log("push request validation tests ok");

// Alert passthrough for a call alert, and VoIP tickets.
const callAlert = alertAps({ ...base, category: "call", interruptionLevel: "time-sensitive", sound: "default" });
assert.equal(callAlert["interruption-level"], "time-sensitive");
assert.equal(callAlert.category, "call");
assert.equal("interruption-level" in alertAps(base), false);
const voipTicket = await sealTicket(env, { t: "ab".repeat(32), e: "sandbox", k: "voip" });
assert.equal((await request("/push", { ticket: voipTicket, alert: { body: "x" } })).status, 400);
assert.equal((await request("/push", { ticket: voipTicket })).status, 400);
assert.equal((await request("/push", { ticket: voipTicket, voip: {} })).status, 400);
assert.equal((await request("/push", { ticket: voipTicket, voip: { data: [] } })).status, 400);
assert.equal((await request("/push", { ticket: voipTicket, voip: { data: { aps: {} } } })).status, 400);
assert.equal((await request("/push", { ticket: voipTicket, voip: { data: { sealed: "x".repeat(4096) } } })).status, 413);
assert.equal((await request("/push", { ticket, voip: { data: { kind: "call" } } })).status, 400);
assert.equal((await request("/push", { ticket, alert: { body: "x" }, interruptionLevel: "critical" })).status, 400);
assert.equal((await request("/push", { ticket, alert: { body: "x" }, sound: "ding" })).status, 400);
console.log("voip push validation tests ok");

// Old account/device identities may still point to the exact same physical phone.
assert.deepEqual([...latestTicketIndices([
  { t: "AB", e: "sandbox", k: "alert" },
  { t: "ab", e: "sandbox", k: "alert" },
  { t: "ab", e: "production", k: "alert" },
  { t: "ab", e: "sandbox", k: "liveactivity" },
  null,
])], [1, 2, 3]);
const newerTicket = await sealTicket(env, { t: "ab".repeat(32), e: "sandbox", k: "alert" });
const sent: string[] = [];
const batch = await pushBatch(new Request("https://relay.test/push-batch", {
  method: "POST", body: JSON.stringify({ notifications: [
    { ticket, alert: { body: "old-key" } },
    { ticket: newerTicket, alert: { body: "current-key" } },
    { ticket: "invalid" },
  ] }),
}), env, async notification => {
  sent.push(notification.alert!.body!);
  return Response.json({ ok: true });
});
assert.deepEqual(sent, ["current-key"]);
assert.deepEqual(await batch.json(), { results: [
  { index: 0, status: 200, superseded: true }, { index: 1, status: 200 }, { index: 2, status: 403, gone: true },
] });
assert.equal((await request("/push-batch", { notifications: [] })).status, 400);
console.log("physical device deduplication tests ok");
