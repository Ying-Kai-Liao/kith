// Person chats (phase 4a P1): the contact, the sealed queue, the owner's end and undo, and the races between them.
// Every answer is also checked against its route's contract type (KITH_CONTRACT_STRICT), so each route called here is
// held to serve/contract/cloud/contract.json. The cloud never opens a message: bodies here are random bytes sealed to
// nobody, with a real Ed25519 signature over the sig input of the design (§3), written out again below on purpose.

import { describe, expect, it } from "vitest";
import { sha256 } from "@noble/hashes/sha2.js";
import { Webhook } from "svix";
import { sweep } from "../src/api";
import { call, clerkToken, env, hostSocket, newHost, type Json, type TestHost } from "./helpers";
import * as ref from "./ref";

const migration = import.meta.glob<string>("../migrations/0010_contacts.sql", { query: "?raw", import: "default", eager: true });

let seq = 0;
const uid = (name: string) => `user_${name}_${++seq}_${ref.b64url(ref.random(4))}`;

interface Device {
  key: ref.SignKey;
  dk: string;
}
interface User {
  userId: string;
  token: string;
  dev: Device;
}

const newDevice = (): Device => {
  const key = ref.signKey();
  return { key, dk: ref.b64url(key.pub) };
};

/** Registers a device with a box key signed by the device key, as an app of 4a does. */
async function register(user: { userId: string; token: string }, dev: Device, platform = "ios", box = true) {
  const boxKey = ref.b64url(ref.boxKey().pub);
  const body: Json = { name: `${platform} phone`, platform };
  if (box) {
    body.boxKey = boxKey;
    body.boxSig = ref.b64url(ref.sign(dev.key, ref.enc.encode(`kith/pc-box/v1${user.userId}${boxKey}`)));
  }
  return { r: await call("POST", "/v1/devices", { token: user.token, key: dev.key, body }), boxKey };
}

async function newUser(name = "u"): Promise<User> {
  const userId = uid(name);
  const user = { userId, token: await clerkToken(userId, { name }) };
  const dev = newDevice();
  expect((await register(user, dev)).r.status).toBe(200);
  return { ...user, dev };
}

async function claim(user: User, host: TestHost) {
  const c = await call("POST", "/v1/claims", { token: user.token });
  const boxKey = ref.b64url(host.box.pub);
  const sig = ref.b64url(ref.sign(host.sign, ref.enc.encode(ref.claimCanonical(c.body.claimId, c.body.nonce, user.userId, host.cid, boxKey))));
  const r = await call("POST", `/v1/claims/${c.body.claimId}/complete`, {
    token: user.token,
    body: { computerId: host.cid, signKey: ref.b64url(host.sign.pub), boxKey, name: "Studio", platform: "macos", device: "mac", version: "0.1.0", sig },
  });
  expect(r.status).toBe(200);
}

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

async function invite(host: TestHost) {
  const inviteId = `inv_${ref.b64url(ref.random(8))}`;
  const token = ref.b64url(ref.random(32));
  const r = await call("POST", "/v1/host/invites", {
    key: host.sign,
    body: { inviteId, tokenHash: hex(sha256(ref.enc.encode(token))), expiresAt: Date.now() + 3600_000 },
  });
  expect(r.status).toBe(200);
  return { inviteId, token };
}

const accept = (user: User, i: { inviteId: string; token: string }) =>
  call("POST", `/v1/invites/${i.inviteId}/accept`, { token: user.token, body: { token: i.token } });

/** Kai owns a claimed computer; Jasmine accepted his invite, so they have a live contact. */
async function pair() {
  const kai = await newUser("kai");
  const jas = await newUser("jas");
  const host = await newHost();
  await claim(kai, host);
  expect((await accept(jas, await invite(host))).status).toBe(200);
  const { contacts } = (await listContacts(kai)).body;
  return { kai, jas, host, contactId: contacts[0].contactId as string };
}

const listContacts = (u: User) => call("GET", "/v1/contacts", { token: u.token, key: u.dev.key });
const directory = async (u: User, contactId: string) =>
  (await listContacts(u)).body.contacts.find((c: Json) => c.contactId === contactId).directory as Json;

const u64be = (n: number) => {
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, BigInt(n));
  return out;
};
const newMsgId = () => `pm_${ref.b64url(ref.random(16))}`;

/** The send body of the design (§3), sealed to nobody: random ct, random epk/wrap, the real rhash and signature. */
function seal(contactId: string, epoch: number, sender: ref.SignKey, dir: Json, o: { msgId?: string; ctLen?: number; keys?: string[] } = {}) {
  const msgId = o.msgId ?? newMsgId();
  const keys: string[] = o.keys ?? [...dir.devices.map((d: Json) => d.key), ...dir.computers.map((m: Json) => `computer:${m.computerId}`)];
  const recipients = keys.map((key) => ({ key, epk: ref.random(32), wrap: ref.random(48) }));
  const sorted = [...recipients].sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  const rhash = sha256(ref.concat(...sorted.flatMap((r) => [ref.enc.encode(r.key), r.epk, r.wrap])));
  const ct = ref.concat(...Array.from({ length: Math.ceil((o.ctLen ?? 40) / 1024) }, () => ref.random(1024))).subarray(0, o.ctLen ?? 40);
  const sigInput = ref.concat(
    ref.enc.encode("kith/pc/v1"),
    ref.enc.encode(contactId),
    u64be(epoch),
    ref.enc.encode(msgId),
    ref.enc.encode(ref.b64url(sender.pub)),
    rhash,
    sha256(ct),
  );
  return {
    msgId,
    epoch,
    ct: ref.b64url(ct),
    sig: ref.b64url(ref.sign(sender, sigInput)),
    recipients: recipients.map((r) => ({ key: r.key, epk: ref.b64url(r.epk), wrap: ref.b64url(r.wrap) })),
  };
}

const send = (u: User, contactId: string, body: unknown) =>
  call("POST", `/v1/contacts/${contactId}/messages`, { token: u.token, key: u.dev.key, body });
const inbox = (u: User) => call("GET", "/v1/pc/inbox", { token: u.token, key: u.dev.key });
const ack = (u: User, msgIds: string[]) => call("POST", "/v1/pc/ack", { token: u.token, key: u.dev.key, body: { msgIds } });
const end = (u: User, contactId: string, epoch: number) =>
  call("POST", `/v1/contacts/${contactId}/end`, { token: u.token, key: u.dev.key, body: { epoch } });
const undo = (u: User, contactId: string, epoch: number) =>
  call("POST", `/v1/contacts/${contactId}/undo`, { token: u.token, key: u.dev.key, body: { epoch } });

const count = async (sql: string, ...binds: unknown[]) => (await env.DB.prepare(sql).bind(...binds).first<{ n: number }>())!.n;
const queued = (contactId: string) => count("SELECT COUNT(*) AS n FROM pc_messages WHERE contact_id = ?", contactId);
const rowsFor = (userId: string, contactId: string) =>
  count(
    "SELECT COUNT(*) AS n FROM pc_recipients r JOIN pc_messages m ON m.msg_id = r.msg_id WHERE r.recipient_user_id = ? AND m.contact_id = ?",
    userId,
    contactId,
  );
const contactRow = (contactId: string) => env.DB.prepare("SELECT * FROM contacts WHERE contact_id = ?").bind(contactId).first<Json>();

describe("accept makes the contact", () => {
  it("makes one live contact per pair, and a second invite keeps it", async () => {
    const { kai, jas, host, contactId } = await pair();
    const mine = (await listContacts(kai)).body.contacts;
    expect(mine).toHaveLength(1);
    expect(mine[0]).toMatchObject({ role: "inviter", epoch: 1, base: 1, live: true, endedBy: null, other: { userId: jas.userId, name: "jas" } });
    const theirs = (await listContacts(jas)).body.contacts;
    expect(theirs[0]).toMatchObject({ contactId, role: "invitee", other: { userId: kai.userId, name: "kai" } });
    // The directory: both accounts' phones with a box key and Kai's claimed computer.
    expect(mine[0].directory.devices.map((d: Json) => d.userId).sort()).toEqual([jas.userId, kai.userId].sort());
    expect(mine[0].directory.computers).toEqual([
      expect.objectContaining({ userId: kai.userId, computerId: host.cid, signKey: ref.b64url(host.sign.pub), boxKey: ref.b64url(host.box.pub) }),
    ]);
    expect((await accept(jas, await invite(host))).status).toBe(409); // already a member: the contact is untouched
    expect(await count("SELECT COUNT(*) AS n FROM contacts WHERE user_lo IN (?1, ?2) AND user_hi IN (?1, ?2)", kai.userId, jas.userId)).toBe(1);
  });

  it("lists a device only once it has a signed box key", async () => {
    const { kai, contactId } = await pair();
    const oldApp = newDevice();
    expect((await register(kai, oldApp, "ios", false)).r.status).toBe(200);
    const keys = (await directory(kai, contactId)).devices.map((d: Json) => d.key);
    expect(keys).not.toContain(oldApp.dk);
  });
});

describe("device box keys", () => {
  it("is set once; the same again is fine, a different one conflicts, a bad signature is refused", async () => {
    const kai = await newUser("kai");
    const dev = newDevice();
    const first = await register(kai, dev);
    expect(first.r.status).toBe(200);
    const row = await env.DB.prepare("SELECT box_pub FROM devices WHERE sign_pub = ?").bind(dev.dk).first<Json>();
    expect(row?.box_pub).toBe(first.boxKey);
    const body = { name: "iPhone", platform: "ios", boxKey: first.boxKey, boxSig: ref.b64url(ref.sign(dev.key, ref.enc.encode(`kith/pc-box/v1${kai.userId}${first.boxKey}`))) };
    expect((await call("POST", "/v1/devices", { token: kai.token, key: dev.key, body })).status).toBe(200);
    expect((await register(kai, dev)).r.status).toBe(409);
    const other = ref.b64url(ref.boxKey().pub);
    const bad = { ...body, boxKey: other };
    expect((await call("POST", "/v1/devices", { token: kai.token, key: dev.key, body: bad })).status).toBe(400);
    // A re-register without the key (an older app) leaves it alone.
    expect((await register(kai, dev, "ios", false)).r.status).toBe(200);
    expect((await env.DB.prepare("SELECT box_pub FROM devices WHERE sign_pub = ?").bind(dev.dk).first<Json>())?.box_pub).toBe(first.boxKey);
  });
});

describe("send, inbox and ack", () => {
  it("queues one row per recipient, hands each its own wrap and keeps the message until the last ack", async () => {
    const { kai, jas, host, contactId } = await pair();
    const dir = await directory(kai, contactId);
    const m = seal(contactId, 1, kai.dev.key, dir);
    const r = await send(kai, contactId, m);
    expect(r).toEqual({ status: 200, body: { status: "ok", msgId: m.msgId, epoch: 1 } });
    expect(await count("SELECT COUNT(*) AS n FROM pc_recipients WHERE msg_id = ?", m.msgId)).toBe(3);

    const theirs = (await inbox(jas)).body;
    expect(theirs.messages).toHaveLength(1);
    const want = m.recipients.find((x) => x.key === jas.dev.dk)!;
    expect(theirs.messages[0]).toMatchObject({ msgId: m.msgId, contactId, epoch: 1, senderUserId: kai.userId, senderKey: kai.dev.dk, ct: m.ct, sig: m.sig, epk: want.epk, wrap: want.wrap });
    const hostInbox = (await call("GET", "/v1/host/pc/inbox", { key: host.sign })).body;
    expect(hostInbox.messages.map((x: Json) => x.msgId)).toEqual([m.msgId]);

    expect((await ack(jas, [m.msgId])).body).toEqual({ ok: true });
    expect((await inbox(jas)).body.messages).toEqual([]);
    expect(await queued(contactId)).toBe(1);
    expect((await call("POST", "/v1/host/pc/ack", { key: host.sign, body: { msgIds: [m.msgId] } })).body).toEqual({ ok: true });
    expect(await queued(contactId)).toBe(1); // Kai's own phone still has it
    expect((await ack(kai, [m.msgId])).status).toBe(200);
    expect(await queued(contactId)).toBe(0);
    expect(await count("SELECT COUNT(*) AS n FROM pc_recipients WHERE msg_id = ?", m.msgId)).toBe(0);
  });

  it("answers a resend as a duplicate and adds nothing; the same id with another body conflicts", async () => {
    const { kai, contactId } = await pair();
    const m = seal(contactId, 1, kai.dev.key, await directory(kai, contactId));
    expect((await send(kai, contactId, m)).body.status).toBe("ok");
    expect((await send(kai, contactId, m)).body).toMatchObject({ status: "duplicate", msgId: m.msgId, epoch: 1 });
    expect(await count("SELECT COUNT(*) AS n FROM pc_recipients WHERE msg_id = ?", m.msgId)).toBe(3);
    const other = seal(contactId, 1, kai.dev.key, await directory(kai, contactId), { msgId: m.msgId });
    expect((await send(kai, contactId, other)).status).toBe(409);
  });

  it("pages the inbox with a cursor, oldest first", async () => {
    const { kai, jas, contactId } = await pair();
    const dir = await directory(kai, contactId);
    const ids: string[] = [];
    for (let i = 0; i < 3; i++) {
      const m = seal(contactId, 1, kai.dev.key, dir);
      ids.push(m.msgId);
      await send(kai, contactId, m);
    }
    const all = (await inbox(jas)).body;
    expect(all.messages.map((x: Json) => x.msgId).sort()).toEqual([...ids].sort());
    expect(all.next).toBeNull();
    const after = `${all.messages[0].createdAt}.${all.messages[0].msgId}`;
    const rest = (await call("GET", `/v1/pc/inbox?after=${after}`, { token: jas.token, key: jas.dev.key })).body;
    expect(rest.messages.map((x: Json) => x.msgId)).toEqual(all.messages.slice(1).map((x: Json) => x.msgId));
    expect((await call("GET", "/v1/pc/inbox?after=nope", { token: jas.token, key: jas.dev.key })).status).toBe(400);
  });

  it("sends from the Mac with its own key, and tells the other computers pc.new, not cloud.changed", async () => {
    const { kai, jas, host, contactId } = await pair();
    const dir = await directory(kai, contactId);
    const jasMac = await newHost();
    await claim(jas, jasMac);
    const sock = await hostSocket(jasMac);
    const m = seal(contactId, 1, host.sign, dir);
    const r = await call("POST", `/v1/host/contacts/${contactId}/messages`, { key: host.sign, body: m });
    expect(r.body.status).toBe("recipientsChanged"); // Jasmine's new Mac joined the directory
    const fresh = seal(contactId, 1, host.sign, await directory(kai, contactId));
    expect((await call("POST", `/v1/host/contacts/${contactId}/messages`, { key: host.sign, body: fresh })).body.status).toBe("ok");
    await sock.nextT("pc.new");
    await sock.none((x) => x.t === "cloud.changed");
    const got = (await inbox(jas)).body.messages[0];
    expect(got.senderKey).toBe(ref.b64url(host.sign.pub));
    expect(got.senderUserId).toBe(kai.userId);
    sock.ws.close();
  });

  it("refuses a bad signature, a stranger's key, a missing recipient (recipientsChanged) and too many or too big", async () => {
    const { kai, jas, contactId } = await pair();
    const dir = await directory(kai, contactId);
    const m = seal(contactId, 1, kai.dev.key, dir);
    // Signed by the wrong key.
    expect((await send(kai, contactId, seal(contactId, 1, ref.signKey(), dir))).status).toBe(400);
    // A tampered recipient set no longer matches the signature.
    const swapped = { ...m, recipients: m.recipients.map((r, i) => (i ? r : { ...r, wrap: ref.b64url(ref.random(48)) })) };
    expect((await send(kai, contactId, swapped)).status).toBe(400);
    // A live device of someone else.
    const stranger = await newUser("stranger");
    const withStranger = seal(contactId, 1, kai.dev.key, dir, { keys: [...dir.devices.map((d: Json) => d.key), stranger.dev.dk] });
    expect((await send(kai, contactId, withStranger)).status).toBe(400);
    // A set that left a device out is the client's to redo, with the fresh directory.
    const short = seal(contactId, 1, kai.dev.key, dir, { keys: dir.devices.slice(1).map((d: Json) => d.key) });
    const r = await send(kai, contactId, short);
    expect(r.body).toMatchObject({ status: "recipientsChanged", msgId: short.msgId, directory: dir });
    // Too big.
    expect((await send(kai, contactId, seal(contactId, 1, kai.dev.key, dir, { ctLen: 64 * 1024 + 1 }))).status).toBe(413);
    expect((await send(kai, contactId, seal(contactId, 1, kai.dev.key, dir, { ctLen: 64 * 1024 }))).body.status).toBe("ok");
    // A stranger to the contact can't send at all.
    expect((await send(stranger, contactId, m)).status).toBe(404);
    expect(await rowsFor(jas.userId, contactId)).toBe(1);
  });

  it("limits sends per device per minute, and queued rows and bytes per account", async () => {
    const { kai, contactId } = await pair();
    const dir = await directory(kai, contactId);
    const insert = env.DB.prepare(
      "INSERT INTO pc_messages(msg_id, contact_id, epoch, sender_user_id, sender_key, ct, sig, rhash, created_at, expires_at) VALUES (?1, ?2, 1, ?3, ?4, zeroblob(16), zeroblob(64), zeroblob(32), ?5, ?6)",
    );
    const now = Date.now();
    await env.DB.batch(
      Array.from({ length: 60 }, (_, i) => insert.bind(`pm_fill${String(i).padStart(18, "0")}`, contactId, kai.userId, kai.dev.dk, now, now + 1e9)),
    );
    expect((await send(kai, contactId, seal(contactId, 1, kai.dev.key, dir))).status).toBe(429);
    await env.DB.prepare("DELETE FROM pc_messages WHERE msg_id LIKE 'pm_fill%'").run();

    // 4,999 rows already queued under this account: three more would pass 5,000.
    await insert.bind("pm_rows000000000000000000", contactId, kai.userId, "someone-else", now - 120_000, now + 1e9).run();
    await env.DB.prepare(
      `WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 4999)
       INSERT INTO pc_recipients(msg_id, recipient_key, recipient_user_id, epk, wrap) SELECT 'pm_rows000000000000000000', 'k' || i, 'x', zeroblob(32), zeroblob(48) FROM n`,
    ).run();
    const full = await send(kai, contactId, seal(contactId, 1, kai.dev.key, dir));
    expect(full.status).toBe(429);
    expect(full.body.error.code).toBe("full");
  });
});

describe("ending a contact (owner route)", () => {
  it("ends at once: contact, revocation, cuts, and only the other side's queue", async () => {
    const { kai, jas, host, contactId } = await pair();
    // Jasmine's phone has access to Kai's computer.
    const nD = ref.random(32);
    const req = await call("POST", `/v1/computers/${host.cid}/access-requests`, { token: jas.token, key: jas.dev.key, body: { commit: ref.b64url(ref.sasCommit(jas.dev.key.pub, nD)) } });
    await call("POST", `/v1/host/access-requests/${req.body.requestId}/nonce`, { key: host.sign, body: { nonce: ref.b64url(ref.random(32)) } });
    await call("POST", `/v1/access-requests/${req.body.requestId}/reveal`, { token: jas.token, key: jas.dev.key, body: { nonce: ref.b64url(nD) } });
    expect((await call("POST", `/v1/host/access-requests/${req.body.requestId}/decision`, { key: host.sign, body: { decision: "approve" } })).status).toBe(200);

    const m = seal(contactId, 1, kai.dev.key, await directory(kai, contactId));
    await send(kai, contactId, m);
    const r = await end(kai, contactId, 1);
    expect(r).toMatchObject({ status: 200, body: { status: "ok", epoch: 1, live: false } });

    expect(await contactRow(contactId)).toMatchObject({ live: 0, ended_by: `owner:${kai.userId}`, made_rev: 1, pgen: 1 });
    expect(await count("SELECT COUNT(*) AS n FROM revocations WHERE owner_user_id = ? AND user_id = ?", kai.userId, jas.userId)).toBe(1);
    const state = (await call("GET", "/v1/host/state", { key: host.sign })).body;
    expect(state.revoked).toEqual([jas.userId]);
    expect(state.memberships).toEqual([]);
    expect(state.grants).toEqual([]);
    expect(state.contacts).toEqual([{ userId: jas.userId, contactId, epoch: 1, base: 1, live: false, pgen: 1, endedBy: `owner:${kai.userId}` }]);
    // Her queue is gone; his own copies (the remover's) stay to collect.
    expect(await rowsFor(jas.userId, contactId)).toBe(0);
    expect(await rowsFor(kai.userId, contactId)).toBe(2);
    expect((await inbox(jas)).body.messages).toEqual([]);
    expect((await inbox(kai)).body.messages).toHaveLength(1);
    // She sees it ended by them; he sees it ended by him; no directory on either.
    const hers = (await listContacts(jas)).body.contacts[0];
    expect(hers).toMatchObject({ live: false, endedBy: "them" });
    expect(hers.directory).toBeUndefined();
    expect((await listContacts(kai)).body.contacts[0].endedBy).toBe("me");
    // The invite route still refuses her (the re-add guard).
    expect((await accept(jas, await invite(host))).status).toBe(410);
  });

  it("only the inviter may end it, and a repeat says alreadyEnded", async () => {
    const { kai, jas, contactId } = await pair();
    expect((await end(jas, contactId, 1)).status).toBe(403);
    expect((await end(kai, contactId, 1)).body.status).toBe("ok");
    expect((await end(kai, contactId, 1)).body).toMatchObject({ status: "alreadyEnded", epoch: 1, live: false });
    expect(await count("SELECT COUNT(*) AS n FROM audit_events WHERE action = 'contact.end' AND target = ?", jas.userId)).toBe(1);
    expect((await end(kai, "ct_nopenopenopenopenopenop", 1)).status).toBe(404);
  });

  it("refuses a send composed for the ended epoch and queues nothing for the other side", async () => {
    const { kai, jas, contactId } = await pair();
    const dir = await directory(kai, contactId);
    await end(kai, contactId, 1);
    const m = seal(contactId, 1, kai.dev.key, dir);
    expect((await send(jas, contactId, seal(contactId, 1, jas.dev.key, dir))).body.status).toBe("contactEnded");
    expect((await send(kai, contactId, m)).body.status).toBe("contactEnded");
    expect(await queued(contactId)).toBe(0);
  });

  it("holds when end and sends race: nothing is left queued for the other side", async () => {
    const { kai, jas, contactId } = await pair();
    const dir = await directory(kai, contactId);
    const sends = Array.from({ length: 6 }, (_, i) => send(i % 2 ? jas : kai, contactId, seal(contactId, 1, (i % 2 ? jas : kai).dev.key, dir)));
    const [ended] = await Promise.all([end(kai, contactId, 1), ...sends]);
    expect(ended.body.status).toBe("ok");
    expect(await rowsFor(jas.userId, contactId)).toBe(0);
    // Any message still there is Kai's own copy, and each has at least one recipient row.
    expect(await count("SELECT COUNT(*) AS n FROM pc_messages m WHERE NOT EXISTS (SELECT 1 FROM pc_recipients r WHERE r.msg_id = m.msg_id)")).toBe(0);
  });
});

describe("undo", () => {
  it("brings the contact, her memberships and grants back at a new epoch with the base kept", async () => {
    const { kai, jas, host, contactId } = await pair();
    await end(kai, contactId, 1);
    const r = await undo(kai, contactId, 1);
    expect(r).toMatchObject({ status: 200, body: { status: "ok", epoch: 2, live: true } });
    expect(await contactRow(contactId)).toMatchObject({ live: 1, epoch: 2, base: 1, ended_by: null, ended_at: null });
    const state = (await call("GET", "/v1/host/state", { key: host.sign })).body;
    expect(state.revoked).toEqual([]);
    expect(state.memberships).toEqual([expect.objectContaining({ userId: jas.userId })]);
    expect(state.contacts[0]).toMatchObject({ epoch: 2, base: 1, live: true });
    // Sends compose against the new epoch; the old one is refused.
    const dir = await directory(kai, contactId);
    expect((await send(kai, contactId, seal(contactId, 1, kai.dev.key, dir))).body).toMatchObject({ status: "staleEpoch", epoch: 2 });
    expect((await send(kai, contactId, seal(contactId, 2, kai.dev.key, dir))).body.status).toBe("ok");
  });

  it("a late duplicate end after undo changes nothing (staleEpoch), and a late undo after a later end does too", async () => {
    const { kai, contactId } = await pair();
    await end(kai, contactId, 1);
    await undo(kai, contactId, 1);
    expect((await end(kai, contactId, 1)).body).toMatchObject({ status: "staleEpoch", epoch: 2, live: true });
    expect(await contactRow(contactId)).toMatchObject({ live: 1, epoch: 2 });
    // A retried undo of the first end says it stands.
    expect((await undo(kai, contactId, 1)).body).toMatchObject({ status: "alreadyLive", epoch: 2, live: true });
    // A real second end, then the late copy of the first undo must not bring it back.
    expect((await end(kai, contactId, 2)).body.status).toBe("ok");
    expect((await undo(kai, contactId, 1)).body).toMatchObject({ status: "staleEpoch", epoch: 2, live: false });
    expect(await contactRow(contactId)).toMatchObject({ live: 0, epoch: 2 });
    expect((await undo(kai, contactId, 2)).body.status).toBe("ok");
  });

  it("is for the inviter on a native device, never a web one", async () => {
    const { kai, jas, contactId } = await pair();
    await end(kai, contactId, 1);
    expect((await undo(jas, contactId, 1)).status).toBe(403);
    const web = newDevice();
    await env.DB.prepare(
      "INSERT INTO devices(id, owner_user_id, sign_pub, name, platform, created_at) VALUES (?, ?, ?, 'Browser', 'web', 1)",
    )
      .bind(`dev_${ref.b64url(ref.random(8))}`, kai.userId, web.dk)
      .run();
    const asWeb = await call("POST", `/v1/contacts/${contactId}/undo`, { token: kai.token, key: web.key, body: { epoch: 1 } });
    expect(asWeb.status).toBe(403);
    expect((await contactRow(contactId))?.live).toBe(0);
  });

  it("ending is allowed from a web device (it only narrows access)", async () => {
    const { kai, contactId } = await pair();
    const web = newDevice();
    await env.DB.prepare(
      "INSERT INTO devices(id, owner_user_id, sign_pub, name, platform, created_at) VALUES (?, ?, ?, 'Browser', 'web', 1)",
    )
      .bind(`dev_${ref.b64url(ref.random(8))}`, kai.userId, web.dk)
      .run();
    const r = await call("POST", `/v1/contacts/${contactId}/end`, { token: kai.token, key: web.key, body: { epoch: 1 } });
    expect(r.body.status).toBe("ok");
  });
});

describe("a host's person revoke: pause keeps the chat, remove ends it", () => {
  const revoke = (host: TestHost, userId: string, extra: Json = {}) =>
    call("POST", "/v1/host/people/revoke", { key: host.sign, body: { userId, ...extra } });
  const unrevoke = (host: TestHost, userId: string, extra: Json = {}) =>
    call("POST", "/v1/host/people/unrevoke", { key: host.sign, body: { userId, ...extra } });
  const stateOf = async (host: TestHost) => (await call("GET", "/v1/host/state", { key: host.sign })).body;

  it("a revoke with no mode (an old host, or an unknown word) pauses: the cuts and the row, the contact and chat stay", async () => {
    const { kai, jas, host, contactId } = await pair();
    expect((await revoke(host, jas.userId)).body).toEqual({ ok: true, status: "ok", epoch: 1, live: true, pgen: 1 });
    expect((await revoke(host, jas.userId, { mode: "banish" })).body.status).toBe("ok");
    expect(await contactRow(contactId)).toMatchObject({ live: 1, epoch: 1, pgen: 2, made_rev: 0 });
    const st = await stateOf(host);
    expect(st.revoked).toEqual([jas.userId]);
    expect(st.memberships).toEqual([]);
    expect(st.contacts[0]).toMatchObject({ live: true, pgen: 2 });
    expect((await listContacts(kai)).body.contacts[0]).toMatchObject({ live: true, paused: true });
    // Chatting goes on while paused.
    expect((await send(jas, contactId, seal(contactId, 1, jas.dev.key, await directory(jas, contactId)))).body.status).toBe("ok");
    expect(await count("SELECT COUNT(*) AS n FROM audit_events WHERE action = 'contact.end' AND target = ?", jas.userId)).toBe(0);
    // Resume gives the agents back and the row goes.
    expect((await unrevoke(host, jas.userId, { pgen: 2 })).body).toEqual({ ok: true, status: "ok", epoch: 1, live: true, pgen: 3 });
    expect((await stateOf(host)).revoked).toEqual([]);
    expect((await stateOf(host)).memberships).toEqual([expect.objectContaining({ userId: jas.userId })]);
    expect((await listContacts(kai)).body.contacts[0].paused).toBe(false);
  });

  it("names the pause generation: a late copy of an older pause or resume changes nothing", async () => {
    const { jas, host, contactId } = await pair();
    expect((await revoke(host, jas.userId, { pgen: 0 })).body.status).toBe("ok"); // pgen 1
    expect((await revoke(host, jas.userId, { pgen: 0 })).body).toEqual({ ok: false, status: "stalePause", epoch: 1, live: true, pgen: 1 });
    expect((await unrevoke(host, jas.userId, { pgen: 1 })).body.status).toBe("ok"); // pgen 2, active
    // A late copy of the first resume must not un-pause a newer pause.
    expect((await revoke(host, jas.userId, { pgen: 2 })).body.status).toBe("ok"); // pgen 3, paused
    expect((await unrevoke(host, jas.userId, { pgen: 1 })).body.status).toBe("stalePause");
    expect((await stateOf(host)).revoked).toEqual([jas.userId]);
    expect(await contactRow(contactId)).toMatchObject({ pgen: 3, live: 1 });
  });

  it("remove needs its epoch, ends the contact and makes the row; the undo takes it away again", async () => {
    const { jas, host, contactId } = await pair();
    expect((await revoke(host, jas.userId, { mode: "remove" })).status).toBe(400);
    expect((await revoke(host, jas.userId, { mode: "remove", epoch: 1 })).body).toEqual({ ok: true, status: "ok", epoch: 1, live: false, pgen: 1 });
    expect(await contactRow(contactId)).toMatchObject({ live: 0, made_rev: 1, ended_by: `computer:${host.cid}` });
    expect((await revoke(host, jas.userId, { mode: "remove", epoch: 1 })).body.status).toBe("alreadyEnded");
    expect((await unrevoke(host, jas.userId, { epoch: 1 })).body).toMatchObject({ ok: true, status: "ok", epoch: 2, live: true });
    expect(await count("SELECT COUNT(*) AS n FROM revocations WHERE user_id = ?", jas.userId)).toBe(0);
    expect((await stateOf(host)).memberships).toEqual([expect.objectContaining({ userId: jas.userId })]);
    // An old host's undo (no epoch) of an ended contact means the ended one.
    await revoke(host, jas.userId, { mode: "remove", epoch: 2 });
    expect((await unrevoke(host, jas.userId)).body).toMatchObject({ status: "ok", epoch: 3, live: true });
  });

  it("pause, then remove, then undo leaves her paused; only a resume gives the agents back", async () => {
    const { kai, jas, host, contactId } = await pair();
    await revoke(host, jas.userId); // paused, pgen 1
    expect((await end(kai, contactId, 1)).body.status).toBe("ok");
    expect(await contactRow(contactId)).toMatchObject({ live: 0, made_rev: 0, pgen: 1 });
    expect((await undo(kai, contactId, 1)).body).toMatchObject({ status: "ok", epoch: 2, live: true });
    expect(await contactRow(contactId)).toMatchObject({ live: 1, epoch: 2, pgen: 1, made_rev: 0 });
    const st = await stateOf(host);
    expect(st.revoked).toEqual([jas.userId]);
    expect(st.memberships).toEqual([]);
    expect((await listContacts(kai)).body.contacts[0].paused).toBe(true);
    expect((await unrevoke(host, jas.userId, { pgen: 1 })).body.status).toBe("ok");
    expect((await stateOf(host)).memberships).toEqual([expect.objectContaining({ userId: jas.userId })]);
  });

  it("refuses a pause or resume while removed, and a resume never lets an ended contact lose its row", async () => {
    const { kai, jas, host, contactId } = await pair();
    await end(kai, contactId, 1);
    expect((await revoke(host, jas.userId)).body).toEqual({ ok: false, status: "removed", epoch: 1, live: false, pgen: 1 });
    expect((await unrevoke(host, jas.userId, { pgen: 1 })).body).toMatchObject({ ok: false, status: "removed" });
    expect(await count("SELECT COUNT(*) AS n FROM revocations WHERE user_id = ?", jas.userId)).toBe(1);
    expect((await accept(jas, await invite(host))).status).toBe(410);
    expect(contactId).toBeTruthy();
  });

  it("holds C-R when a pause and a remove race: an ended contact always has its row", async () => {
    const { kai, jas, host, contactId } = await pair();
    await Promise.all([revoke(host, jas.userId), end(kai, contactId, 1), revoke(host, jas.userId, { pgen: 0 })]);
    const k = await contactRow(contactId);
    expect(await count("SELECT COUNT(*) AS n FROM revocations WHERE user_id = ?", jas.userId)).toBe(1);
    if (!k!.live) expect(k!.ended_by).toMatch(/^owner:/);
    expect(await count("SELECT COUNT(*) AS n FROM memberships WHERE user_id = ? AND revoked_at IS NULL", jas.userId)).toBe(0);
  });

  it("a person with no contact is revoked as before", async () => {
    const { host } = await pair();
    const nobody = await newUser("nobody");
    expect((await revoke(host, nobody.userId)).body).toEqual({ ok: true, status: null, epoch: null, live: null, pgen: null });
  });

  it("revoking one computer's membership does not end the contact", async () => {
    const { host, contactId } = await pair();
    const state = await stateOf(host);
    expect((await call("POST", `/v1/host/memberships/${state.memberships[0].membershipId}/revoke`, { key: host.sign, body: {} })).status).toBe(200);
    expect((await contactRow(contactId))?.live).toBe(1);
  });
});

describe("devices and computers that go", () => {
  it("an unclaimed computer's queued rows go, and a message with none left goes with them", async () => {
    const { kai, jas, host, contactId } = await pair();
    const dir = await directory(kai, contactId);
    const m = seal(contactId, 1, kai.dev.key, dir);
    await send(kai, contactId, m);
    for (const u of [kai, jas]) await ack(u, [m.msgId]);
    expect(await queued(contactId)).toBe(1);
    // The computer is the last recipient: unclaiming it drops its row and then the message.
    expect((await call("DELETE", `/v1/computers/${host.cid}`, { token: kai.token })).status).toBe(200);
    expect(await queued(contactId)).toBe(0);
  });

  it("revoking a phone drops its rows", async () => {
    const { kai, jas, contactId } = await pair();
    const m = seal(contactId, 1, kai.dev.key, await directory(kai, contactId));
    await send(kai, contactId, m);
    const id = (await call("GET", "/v1/devices", { token: jas.token })).body.devices[0].deviceId;
    expect((await call("DELETE", `/v1/devices/${id}`, { token: jas.token })).status).toBe(200);
    expect(await rowsFor(jas.userId, contactId)).toBe(0);
    expect(await rowsFor(kai.userId, contactId)).toBe(2); // his phone and his computer
  });

  it("a deleted account ends its contacts and drops its own rows; the other side keeps its copies", async () => {
    const { kai, jas, contactId } = await pair();
    await send(kai, contactId, seal(contactId, 1, kai.dev.key, await directory(kai, contactId)));
    const payload = { type: "user.deleted", data: { id: jas.userId } };
    const id = `msg_${ref.b64url(ref.random(8))}`;
    const now = new Date();
    const r = await call("POST", "/v1/webhooks/clerk", {
      body: payload,
      headers: {
        "svix-id": id,
        "svix-timestamp": String(Math.floor(now.getTime() / 1000)),
        "svix-signature": new Webhook(env.CLERK_WEBHOOK_SECRET!).sign(id, now, JSON.stringify(payload)),
      },
    });
    expect(r.status).toBe(200);
    expect(await contactRow(contactId)).toMatchObject({ live: 0, ended_by: "accountDeleted" });
    expect(await rowsFor(jas.userId, contactId)).toBe(0);
    expect(await rowsFor(kai.userId, contactId)).toBe(2);
    expect((await listContacts(kai)).body.contacts[0].endedBy).toBe("system");
  });
});

describe("the 30-day sweep", () => {
  it("deletes expired messages with their rows and leaves the rest", async () => {
    const { kai, contactId } = await pair();
    const dir = await directory(kai, contactId);
    const keep = seal(contactId, 1, kai.dev.key, dir);
    const old = seal(contactId, 1, kai.dev.key, dir);
    await send(kai, contactId, keep);
    await send(kai, contactId, old);
    await env.DB.prepare("UPDATE pc_messages SET expires_at = ? WHERE msg_id = ?").bind(Date.now() - 1, old.msgId).run();
    await sweep(env, Date.now());
    expect(await count("SELECT COUNT(*) AS n FROM pc_messages WHERE msg_id = ?", old.msgId)).toBe(0);
    expect(await count("SELECT COUNT(*) AS n FROM pc_recipients WHERE msg_id = ?", old.msgId)).toBe(0);
    expect(await count("SELECT COUNT(*) AS n FROM pc_recipients WHERE msg_id = ?", keep.msgId)).toBe(3);
  });

  it("expires 30 days after the send", async () => {
    const { kai, contactId } = await pair();
    const m = seal(contactId, 1, kai.dev.key, await directory(kai, contactId));
    const before = Date.now();
    await send(kai, contactId, m);
    const row = await env.DB.prepare("SELECT created_at, expires_at FROM pc_messages WHERE msg_id = ?").bind(m.msgId).first<Json>();
    expect(row!.expires_at - row!.created_at).toBe(30 * 86_400_000);
    expect(row!.created_at).toBeGreaterThanOrEqual(before);
  });
});

describe("backfill (0010_contacts.sql) on old data", () => {
  it("makes live contacts from memberships and ended ones from revocations, once per pair", async () => {
    const sql = Object.values(migration)[0]!;
    const statements = sql
      .split("\n")
      .map((l) => l.replace(/--.*$/, ""))
      .join("\n")
      .split(";")
      .map((s) => s.trim())
      .filter((s) => /^(INSERT OR IGNORE INTO contacts|UPDATE contacts)/.test(s));
    expect(statements).toHaveLength(3);

    const tag = ref.b64url(ref.random(4));
    const id = (n: string) => `bf_${n}_${tag}`;
    const accounts = ["oa", "ma", "ob", "mb", "oc", "mc", "od", "md"].map(id);
    for (const a of accounts) await env.DB.prepare("INSERT INTO accounts(user_id, created_at) VALUES (?, 1)").bind(a).run();
    const computer = async (n: string, owner: string) => {
      const k = ref.b64url(ref.random(32));
      await env.DB.prepare(
        "INSERT INTO computers(id, sign_pub, box_pub, owner_user_id, name, platform, version, created_at, updated_at) VALUES (?, ?, ?, ?, 'Mac', 'macos', '0', 1, 1)",
      )
        .bind(id(`cmp${n}`), k, k, owner)
        .run();
      await env.DB.prepare("INSERT INTO invites(invite_id, computer_id, token_hash, expires_at, created_at) VALUES (?, ?, 'h', 9, 1)")
        .bind(id(`inv${n}`), id(`cmp${n}`))
        .run();
      return id(`cmp${n}`);
    };
    const member = (n: string, cmp: string, user: string, revokedAt: number | null, createdAt = 100) =>
      env.DB.prepare("INSERT INTO memberships(membership_id, computer_id, user_id, invite_id, created_at, revoked_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(id(`mem${n}`), cmp, user, id(`inv${n}`), createdAt, revokedAt)
        .run();
    const revoke = (owner: string, user: string, at: number) =>
      env.DB.prepare("INSERT INTO revocations(owner_user_id, user_id, revoked_at) VALUES (?, ?, ?)").bind(owner, user, at).run();

    // A: an active membership. B: a revocation and nothing else. C: an active membership that is also revoked.
    // D: a membership the owner ended on that computer alone: no contact.
    await member("a", await computer("a", id("oa")), id("ma"), null);
    await revoke(id("ob"), id("mb"), 555);
    await member("c", await computer("c", id("oc")), id("mc"), null);
    await revoke(id("oc"), id("mc"), 777);
    await member("d", await computer("d", id("od")), id("md"), 5);

    for (const s of statements) await env.DB.prepare(s).run();
    for (const s of statements) await env.DB.prepare(s).run(); // idempotent

    const pairOf = (a: string, b: string) => (a < b ? [a, b] : [b, a]);
    const get = (a: string, b: string) => {
      const [lo, hi] = pairOf(id(a), id(b));
      return env.DB.prepare("SELECT * FROM contacts WHERE user_lo = ? AND user_hi = ?").bind(lo, hi).all<Json>();
    };
    const a = (await get("oa", "ma")).results;
    expect(a).toHaveLength(1);
    expect(a[0]).toMatchObject({ inviter_user_id: id("oa"), epoch: 1, base: 1, live: 1, ended_by: null, made_rev: 0, pgen: 0 });
    expect(a[0].contact_id).toMatch(/^ct_[0-9a-f]{32}$/);
    const b = (await get("ob", "mb")).results;
    expect(b).toHaveLength(1);
    expect(b[0]).toMatchObject({ inviter_user_id: id("ob"), live: 0, ended_by: "backfill", ended_at: 555, made_rev: 1 });
    const c = (await get("oc", "mc")).results;
    expect(c).toHaveLength(1);
    expect(c[0]).toMatchObject({ live: 0, ended_by: "backfill", ended_at: 777, made_rev: 1 });
    expect((await get("od", "md")).results).toHaveLength(0);
    // Invariant C-R for what it made: ended iff revoked.
    for (const [o, m] of [["ob", "mb"], ["oc", "mc"]] as const) {
      expect(await count("SELECT COUNT(*) AS n FROM revocations WHERE owner_user_id = ? AND user_id = ?", id(o), id(m))).toBe(1);
    }
  });
});
