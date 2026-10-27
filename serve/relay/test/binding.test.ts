// Tickets are bound to one computer and expire; the computer proves itself with Kith-Sig.
import assert from "node:assert/strict";
import worker, { openTicket, pushOne, RATE, sealTicket, TICKET_TTL } from "../src/index.ts";

const encoder = new TextEncoder();
const b64url = (bytes: Uint8Array) => Buffer.from(bytes).toString("base64url");
const sha = async (bytes: Uint8Array) => new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));

/** A host identity: its computer id and a Kith-Sig signer, as serve/host's `Identity::sign_request`. */
async function computer() {
  const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]) as CryptoKeyPair;
  const pub = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  const id = b64url((await sha(pub)).subarray(0, 16));
  async function sign(url: string, body: string, ts = Date.now()) {
    const u = new URL(url);
    const nonce = b64url(crypto.getRandomValues(new Uint8Array(16)));
    const input = ["kith-sig-v1", "POST", u.host, u.pathname + u.search, String(ts), nonce, b64url(await sha(encoder.encode(body)))].join("\n");
    const sig = new Uint8Array(await crypto.subtle.sign({ name: "Ed25519" }, pair.privateKey, encoder.encode(input)));
    return `v=1,kid=${b64url(pub)},ts=${ts},nonce=${nonce},sig=${b64url(sig)}`;
  }
  return { id, sign };
}
const A = await computer();
const B = await computer();

// FCM delivery, stubbed at the global fetch the FCM client sends with.
const rsa = await crypto.subtle.generateKey({ name: "RSASSA-PKCS1-v1_5", modulusLength: 2048,
  publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["sign", "verify"]);
const account = { project_id: "test-kith", client_email: "push@test-kith.iam.gserviceaccount.com",
  private_key: "-----BEGIN PRIVATE KEY-----\n" + Buffer.from(await crypto.subtle.exportKey("pkcs8", rsa.privateKey)).toString("base64") + "\n-----END PRIVATE KEY-----" };
let delivered: string[] = [];
let failure: { status: number; code: string } | undefined;
globalThis.fetch = (async (url: string | URL | Request, options?: RequestInit) => {
  if (String(url).startsWith("https://oauth2.googleapis.com/")) return Response.json({ access_token: "test", expires_in: 3600 });
  const message = JSON.parse(String(options?.body)).message;
  if (failure) return Response.json({ error: { status: failure.code, details: [
    { "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError", errorCode: failure.code },
  ] } }, { status: failure.status });
  delivered.push(message.token);
  return Response.json({ name: "ok" });
}) as typeof fetch;

const key = Buffer.from(crypto.getRandomValues(new Uint8Array(32))).toString("base64");
const env = { TICKET_KEY: key, FCM_SERVICE_ACCOUNT: JSON.stringify(account), LEGACY_TICKETS_UNTIL: "2999-01-01" } as any;
const after = { ...env, LEGACY_TICKETS_UNTIL: "2000-01-01" };

async function call(path: string, body: unknown, signer?: typeof A, e = env, tamper = (h: string) => h) {
  const url = `https://relay.test${path}`;
  const text = JSON.stringify(body);
  const headers: Record<string, string> = signer ? { "Kith-Sig": tamper(await signer.sign(url, text)) } : {};
  return worker.fetch(new Request(url, { method: "POST", body: text, headers }), e);
}
const alertFor = (ticket: string, computerId = A.id) =>
  ({ ticket, alert: { title: "Kith", body: "Needs you" }, data: { botId: "bot", computerId, ctx: "local", sealed: "x" } });
async function fcmTicket(token: string, computerId = A.id, e = env) {
  const r = await call("/register", { provider: "fcm", token, kind: "alert", ctx: "local", computerId }, undefined, e);
  assert.equal(r.status, 200);
  return (await r.json() as { ticket: string }).ticket;
}

// Registration mints a ticket for one computer that expires.
const now = Math.floor(Date.now() / 1000);
const ticket = await fcmTicket("device-one-token-0123456789");
const opened = await openTicket(env, ticket);
assert.equal(opened?.h, A.id);
assert.ok(Math.abs(opened!.x! - (now + TICKET_TTL.alert)) < 5);
const apns = { token: "ab".repeat(32), env: "sandbox", kind: "liveactivity" };
const bound = await (await call("/register", { ...apns, computerId: A.id })).json() as { ticket: string };
assert.equal((await openTicket(env, bound.ticket))?.h, A.id);
assert.ok(Math.abs((await openTicket(env, bound.ticket))!.x! - (now + TICKET_TTL.liveactivity)) < 5);
assert.equal((await call("/register", { ...apns, computerId: "not an id" })).status, 400);
// An app from before bound tickets gets a legacy ticket during the grace window, and nothing after it.
const old = await (await call("/register", apns)).json() as { ticket: string };
assert.equal((await openTicket(env, old.ticket))?.x, undefined);
assert.equal((await call("/register", apns, undefined, after)).status, 400);
assert.equal((await call("/register", { ...apns, computerId: A.id }, undefined, after)).status, 200);
console.log("registration tests ok");

// A valid ticket, pushed by its own computer.
let r = await call("/push", alertFor(ticket), A, after);
assert.equal(r.status, 200);
assert.deepEqual(delivered, ["device-one-token-0123456789"]);
// Another computer holding the ticket can't use it, even claiming to be A in the data.
assert.equal((await call("/push", alertFor(ticket), B, after)).status, 403);
assert.equal((await call("/push", alertFor(ticket), B)).status, 403);
// Signatures that don't verify: altered, over another body, outside the time window, unsigned after the grace window.
assert.equal((await call("/push", alertFor(ticket), A, env, h => h.replace(/sig=(.)/, (_, c) => `sig=${c === "A" ? "B" : "A"}`))).status, 403);
assert.equal((await call("/push", alertFor(ticket), A, env, h => h.replace(/ts=\d+/, `ts=${Date.now() - 600_000}`))).status, 403);
{
  const url = "https://relay.test/push";
  const header = await A.sign(url, JSON.stringify(alertFor(ticket, B.id)));
  const forged = await worker.fetch(new Request(url, { method: "POST", body: JSON.stringify(alertFor(ticket)), headers: { "Kith-Sig": header } }), env);
  assert.equal(forged.status, 403);
  const late = await A.sign(url, JSON.stringify(alertFor(ticket)), Date.now() - 600_000);
  const replayed = await worker.fetch(new Request(url, { method: "POST", body: JSON.stringify(alertFor(ticket)), headers: { "Kith-Sig": late } }), env);
  assert.equal(replayed.status, 403);
}
assert.equal((await call("/push", alertFor(ticket), undefined, after)).status, 403);
// A ticket altered by its holder doesn't open.
assert.equal((await call("/push", alertFor(ticket.slice(0, -2) + (ticket.endsWith("AA") ? "BB" : "AA")), A)).status, 403);
// Expired: refused, and the host is told to forget it (410 + gone).
const expired = await sealTicket(env, { p: "fcm", t: "device-one-token-0123456789", k: "alert", c: "local", h: A.id, x: now - 1 });
r = await call("/push", alertFor(expired), A);
assert.equal(r.status, 410);
assert.equal((await r.json() as { gone: boolean }).gone, true);
console.log("signed push, wrong computer, bad signature and expiry tests ok");

// Hosts from before signed pushes keep working during the grace window, unless they name another computer.
delivered = [];
assert.equal((await call("/push", alertFor(ticket))).status, 200);
assert.equal((await call("/push", alertFor(ticket, B.id))).status, 403);
// Legacy tickets (no computer, no expiry): accepted before the cutoff, refused and forgotten after it.
const legacy = await sealTicket(env, { p: "fcm", t: "legacy-device-token-0123456789", k: "alert", c: "local", h: A.id });
assert.equal((await call("/push", alertFor(legacy), A)).status, 200);
assert.equal((await call("/push", alertFor(legacy))).status, 200);
r = await call("/push", alertFor(legacy), A, after);
assert.equal(r.status, 410);
assert.equal((await r.json() as { gone: boolean }).gone, true);
assert.equal((await call("/push", { ticket: old.ticket, liveActivity: {} }, A, after)).status, 410);
assert.equal((await call("/push", { ticket: old.ticket, liveActivity: {} }, A)).status, 400, "accepted, then the payload is checked");
assert.deepEqual(delivered, ["device-one-token-0123456789", "legacy-device-token-0123456789", "legacy-device-token-0123456789"]);
console.log("legacy ticket and unsigned host grace window tests ok");

// Rate limit per device: the window's limit, then 429 until the next window.
const busy = await fcmTicket("busy-device-token-0123456789");
const start = Date.now();
const body = alertFor(busy);
for (let i = 0; i < RATE.limit; i++) assert.equal((await pushOne(body, { computerId: A.id }, env, start)).status, 200);
assert.equal((await pushOne(body, { computerId: A.id }, env, start)).status, 429);
assert.equal((await pushOne(alertFor(ticket), { computerId: A.id }, env, start)).status, 200, "other devices are unaffected");
assert.equal((await pushOne(body, { computerId: A.id }, env, start + RATE.windowMs)).status, 200);
console.log("rate limit tests ok");

// The provider says the token is dead: 410 + gone, and the host forgets the ticket. A provider 403 is about the
// relay's own credentials (APNs: ExpiredProviderToken, InvalidProviderToken; FCM: PERMISSION_DENIED), so it keeps the ticket.
failure = { status: 404, code: "UNREGISTERED" };
r = await call("/push", alertFor(ticket), A);
assert.equal(r.status, 410);
assert.equal((await r.json() as { gone: boolean }).gone, true);
failure = { status: 403, code: "PERMISSION_DENIED" };
r = await call("/push", alertFor(ticket), A);
assert.equal(r.status, 502);
assert.equal((await r.json() as { gone: boolean }).gone, false);
failure = undefined;
console.log("provider refusal tests ok");

// A batch is signed once, and each ticket is checked against the signer.
delivered = [];
const other = await fcmTicket("b-device-token-0123456789", B.id);
const stale = await sealTicket(env, { p: "fcm", t: "expired-device-token-012345", k: "alert", c: "local", h: A.id, x: now - 1 });
const batch = { notifications: [alertFor(ticket), alertFor(other, B.id), alertFor(stale), { ticket: "invalid" }] };
r = await call("/push-batch", batch, A, after);
assert.deepEqual(await r.json(), { results: [{ index: 0, status: 200 }, { index: 1, status: 403 }, { index: 2, status: 410 }, { index: 3, status: 403 }] });
assert.deepEqual(delivered, ["device-one-token-0123456789"]);
assert.equal((await call("/push-batch", batch, A, env, h => h.replace(/ts=\d+/, "ts=1"))).status, 403);
r = await call("/push-batch", batch, undefined, after);
assert.deepEqual((await r.json() as { results: Array<{ status: number }> }).results.map(x => x.status), [403, 403, 410, 403]);
console.log("batch binding tests ok");
