// Ticket round-trip: sealed tickets open with the same key and fail with another.
import assert from "node:assert/strict";
import worker, { sealTicket, openTicket, apnsTopic, latestTicketIndices } from "../src/index.ts";

const key = btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(32))));
const env = { TICKET_KEY: key, LEGACY_TICKETS_UNTIL: "2999-01-01" } as any;
const ticket = await sealTicket(env, { t: "ab".repeat(32), e: "sandbox", k: "alert" });
assert.deepEqual(await openTicket(env, ticket), { t: "ab".repeat(32), e: "sandbox", k: "alert" });
const other = { TICKET_KEY: btoa(String.fromCharCode(...new Uint8Array(32))) } as any;
assert.equal(await openTicket(other, ticket), null);
assert.equal(await openTicket(env, ticket.slice(0, -2) + "AA"), null);
console.log("ticket tests ok");

const registration = { token: "ab".repeat(32), env: "sandbox", kind: "alert", bundleId: "com.yingkailiao.kith.ios.dev" };
const response = await worker.fetch(new Request("https://relay.test/register", { method: "POST", body: JSON.stringify(registration) }), env);
assert.equal(response.status, 200);
const registered = await response.json() as { ticket: string };
assert.equal((await openTicket(env, registered.ticket))?.b, registration.bundleId);
assert.equal(apnsTopic(undefined, "alert"), "com.yingkailiao.kith.ios");
assert.equal(apnsTopic(registration.bundleId, "liveactivity"), "com.yingkailiao.kith.ios.dev.push-type.liveactivity");
assert.throws(() => apnsTopic("another.app", "alert"));
const invalid = await worker.fetch(new Request("https://relay.test/register", { method: "POST", body: JSON.stringify({ ...registration, bundleId: "another.app" }) }), env);
assert.equal(invalid.status, 400);
for (const id of ["com.yingkailiao.kith.ios", "com.yingkailiao.kith.ios.dev"]) {
  const ok = await worker.fetch(new Request("https://relay.test/register", { method: "POST", body: JSON.stringify({ ...registration, bundleId: id }) }), env);
  assert.equal(ok.status, 200);
}
assert.deepEqual([...latestTicketIndices([
  { t: "ab", e: "sandbox", k: "alert" },
  { t: "ab", e: "sandbox", k: "alert", b: registration.bundleId },
])], [0, 1]);
console.log("development push destinations stay isolated; existing tickets retain production topic");

// VoIP tickets: APNs only, on the `.voip` topic, with the alert lifetime.
assert.equal(apnsTopic(undefined, "voip"), "com.yingkailiao.kith.ios.voip");
assert.equal(apnsTopic(registration.bundleId, "voip"), "com.yingkailiao.kith.ios.dev.voip");
const voip = await worker.fetch(new Request("https://relay.test/register", { method: "POST", body: JSON.stringify({ ...registration, kind: "voip", computerId: "A".repeat(22) }) }), env);
assert.equal(voip.status, 200);
const voipPayload = await openTicket(env, (await voip.json() as { ticket: string }).ticket);
assert.equal(voipPayload?.k, "voip");
assert.equal(voipPayload && (voipPayload.x! - Math.floor(Date.now() / 1000)) > 89 * 86400, true);
const fcmVoip = await worker.fetch(new Request("https://relay.test/register", { method: "POST", body: JSON.stringify({ provider: "fcm", token: "a".repeat(30), kind: "voip", ctx: "local", computerId: "A".repeat(22) }) }), env);
assert.equal(fcmVoip.status, 400);
console.log("voip tickets ok");
