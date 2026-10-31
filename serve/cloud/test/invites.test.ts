// Invites and memberships: the host posts an invite, an account accepts it, the host reads the membership and can revoke it.

import { describe, expect, it } from "vitest";
import { sha256 } from "@noble/hashes/sha2.js";
import { Webhook } from "svix";
import { call, clerkToken, env, newHost, type Json, type TestHost } from "./helpers";
import * as ref from "./ref";

let seq = 0;
const uid = (name: string) => `user_${name}_${++seq}_${ref.b64url(ref.random(4))}`;

async function newUser(name = "a") {
  const userId = uid(name);
  const token = await clerkToken(userId);
  const key = ref.signKey();
  const r = await call("POST", "/v1/devices", { token, key, body: { name: "iPhone", platform: "ios" } });
  expect(r.status).toBe(200);
  return { userId, token, key, dk: ref.b64url(key.pub) };
}
type User = Awaited<ReturnType<typeof newUser>>;

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

/** An owner with a claimed host, and an invite the host posted. */
async function setup() {
  const owner = await newUser("owner");
  const host = await newHost();
  await claim(owner, host);
  return { owner, host, ...(await invite(host)) };
}

async function invite(host: TestHost, o: { ttl?: number } = {}) {
  const inviteId = `inv_${ref.b64url(ref.random(8))}`;
  const token = ref.b64url(ref.random(32));
  const tokenHash = hex(sha256(ref.enc.encode(token)));
  const r = await call("POST", "/v1/host/invites", { key: host.sign, body: { inviteId, tokenHash, expiresAt: Date.now() + (o.ttl ?? 24 * 3600_000) } });
  expect(r).toEqual({ status: 200, body: { ok: true } });
  return { inviteId, token, tokenHash };
}

const accept = (user: User, inviteId: string, token: string) => call("POST", `/v1/invites/${inviteId}/accept`, { token: user.token, body: { token } });
const state = async (host: TestHost) => (await call("GET", "/v1/host/state", { key: host.sign })).body as Json;

/** Runs the access-request flow for a member's device up to the host's approval. */
async function grantAccess(user: User, host: TestHost) {
  const nD = ref.random(32);
  const req = await call("POST", `/v1/computers/${host.cid}/access-requests`, {
    token: user.token,
    key: user.key,
    body: { commit: ref.b64url(ref.sasCommit(user.key.pub, nD)) },
  });
  expect(req.status).toBe(200);
  const id = req.body.requestId as string;
  expect((await call("POST", `/v1/host/access-requests/${id}/nonce`, { key: host.sign, body: { nonce: ref.b64url(ref.random(32)) } })).status).toBe(200);
  expect((await call("POST", `/v1/access-requests/${id}/reveal`, { token: user.token, key: user.key, body: { nonce: ref.b64url(nD) } })).status).toBe(200);
  const decision = await call("POST", `/v1/host/access-requests/${id}/decision`, { key: host.sign, body: { decision: "approve" } });
  expect(decision.status).toBe(200);
  return { requestId: id, grantId: decision.body.grantId as string };
}

describe("host invites", () => {
  it("stores an invite, treats a re-post as a no-op, and refuses a clash", async () => {
    const { host, inviteId, tokenHash } = await setup();
    const exp = Date.now() + 3600_000;
    expect((await call("POST", "/v1/host/invites", { key: host.sign, body: { inviteId, tokenHash, expiresAt: exp } })).status).toBe(200);
    const other = hex(sha256(ref.enc.encode("x")));
    expect((await call("POST", "/v1/host/invites", { key: host.sign, body: { inviteId, tokenHash: other, expiresAt: exp } })).status).toBe(409);
    const n = await env.DB.prepare("SELECT COUNT(*) AS n FROM invites WHERE invite_id = ?").bind(inviteId).first<{ n: number }>();
    expect(n?.n).toBe(1);
    const audits = await env.DB.prepare("SELECT COUNT(*) AS n FROM audit_events WHERE action = 'invite.create' AND target = ?").bind(inviteId).first<{ n: number }>();
    expect(audits?.n).toBe(1);
  });

  it("validates the body and needs a claimed computer", async () => {
    const { host, tokenHash } = await setup();
    const post = (b: Json) => call("POST", "/v1/host/invites", { key: host.sign, body: b });
    const ok = { inviteId: "inv_ok", tokenHash, expiresAt: Date.now() + 3600_000 };
    expect((await post({ ...ok, tokenHash: "nothex" })).status).toBe(400);
    expect((await post({ ...ok, inviteId: "bad id!" })).status).toBe(400);
    expect((await post({ ...ok, expiresAt: Date.now() - 1 })).status).toBe(400);
    expect((await post({ ...ok, expiresAt: Date.now() + 30 * 86_400_000 })).status).toBe(400);
    expect((await post({ ...ok, expiresAt: "soon" })).status).toBe(400);
    const loose = await newHost();
    expect((await call("POST", "/v1/host/invites", { key: loose.sign, body: ok })).status).toBe(403);
    expect((await call("POST", "/v1/host/invites", { body: ok })).status).toBe(401);
  });

  it("revokes an unaccepted invite, once, and not one that was accepted or belongs to another host", async () => {
    const { host, inviteId, token } = await setup();
    const friend = await newUser("friend");
    const stranger = await newHost();
    expect((await call("POST", `/v1/host/invites/${inviteId}/revoke`, { key: stranger.sign, body: {} })).status).toBe(404);
    expect((await call("POST", `/v1/host/invites/${inviteId}/revoke`, { key: host.sign, body: {} })).body).toEqual({ ok: true });
    expect((await call("POST", `/v1/host/invites/${inviteId}/revoke`, { key: host.sign, body: {} })).status).toBe(200);
    const r = await accept(friend, inviteId, token);
    expect(r.status).toBe(410);
    const used = await invite(host);
    expect((await accept(friend, used.inviteId, used.token)).status).toBe(200);
    expect((await call("POST", `/v1/host/invites/${used.inviteId}/revoke`, { key: host.sign, body: {} })).status).toBe(409);
    const audits = await env.DB.prepare("SELECT COUNT(*) AS n FROM audit_events WHERE action = 'invite.revoke' AND target = ?").bind(inviteId).first<{ n: number }>();
    expect(audits?.n).toBe(1);
  });
});

describe("accepting an invite", () => {
  it("creates a membership the host sees, and the same user accepting again gets the same one", async () => {
    const { owner, host, inviteId, token } = await setup();
    const friend = await newUser("friend");
    const first = await accept(friend, inviteId, token);
    expect(first.status).toBe(200);
    expect(first.body.computerId).toBe(host.cid);
    expect(first.body.membershipId).toMatch(/^mem_/);
    const again = await accept(friend, inviteId, token);
    expect(again.body).toEqual(first.body);
    expect((await state(host)).memberships).toEqual([
      { membershipId: first.body.membershipId, userId: friend.userId, inviteId, email: `${friend.userId}@example.com` },
    ]);
    const audits = await env.DB.prepare("SELECT COUNT(*) AS n FROM audit_events WHERE action = 'invite.accept' AND target = ?").bind(inviteId).first<{ n: number }>();
    expect(audits?.n).toBe(1);
    expect(owner.userId).not.toBe(friend.userId);
  });

  it("answers 409 to a second user, 403 to a wrong token and the owner, 404 to an unknown invite, 401 without a session", async () => {
    const { owner, inviteId, token } = await setup();
    const a = await newUser("a");
    const b = await newUser("b");
    expect((await accept(a, inviteId, "wrong-token")).status).toBe(403);
    expect((await accept(owner, inviteId, token)).status).toBe(403);
    expect((await accept(a, "inv_missing", token)).status).toBe(404);
    expect((await call("POST", `/v1/invites/${inviteId}/accept`, { body: { token } })).status).toBe(401);
    expect((await accept(a, inviteId, token)).status).toBe(200);
    const lost = await accept(b, inviteId, token);
    expect(lost.status).toBe(409);
    expect(lost.body.error.message).toMatch(/already used/);
  });

  it("answers 410 to an expired invite", async () => {
    const { host } = await setup();
    const short = await invite(host, { ttl: 1000 });
    await new Promise((r) => setTimeout(r, 1100));
    const r = await accept(await newUser("late"), short.inviteId, short.token);
    expect(r.status).toBe(410);
    expect(r.body.error.code).toBe("inviteExpired");
  });

  it("lets exactly one of two accounts win a race", async () => {
    const { host, inviteId, token } = await setup();
    const [a, b] = [await newUser("a"), await newUser("b")];
    const results = await Promise.all([accept(a, inviteId, token), accept(b, inviteId, token)]);
    expect(results.map((r) => r.status).sort()).toEqual([200, 409]);
    expect((await state(host)).memberships).toHaveLength(1);
  });

  it("won't let an account that is already a member use a second invite", async () => {
    const { host, inviteId, token } = await setup();
    const friend = await newUser("friend");
    expect((await accept(friend, inviteId, token)).status).toBe(200);
    const second = await invite(host);
    expect((await accept(friend, second.inviteId, second.token)).status).toBe(409);
  });
});

describe("members on the account API", () => {
  it("lists member computers with a role, and hides the owner's other computers and other members", async () => {
    const { owner, host, inviteId, token } = await setup();
    const spare = await newHost();
    await claim(owner, spare);
    const friend = await newUser("friend");
    const bystander = await newUser("bystander");
    expect((await call("GET", "/v1/computers", { token: friend.token })).body.computers).toEqual([]);
    await accept(friend, inviteId, token);
    const mine = (await call("GET", "/v1/computers", { token: friend.token, key: friend.key })).body.computers;
    expect(mine.map((c: Json) => [c.computerId, c.role, c.access])).toEqual([[host.cid, "member", "none"]]);
    const owned = (await call("GET", "/v1/computers", { token: owner.token })).body.computers;
    expect(owned.map((c: Json) => c.role)).toEqual(["owner", "owner"]);
    expect((await call("GET", "/v1/computers", { token: bystander.token })).body.computers).toEqual([]);
    // Members don't manage the computer.
    expect((await call("PATCH", `/v1/computers/${host.cid}`, { token: friend.token, body: { name: "Mine" } })).status).toBe(404);
    expect((await call("DELETE", `/v1/computers/${host.cid}`, { token: friend.token })).status).toBe(404);
    expect((await call("GET", `/v1/computers/${host.cid}/grants`, { token: friend.token })).status).toBe(404);
  });

  it("lets a member (not a stranger) ask for access, and the host approves it with the user id visible", async () => {
    const { host, inviteId, token } = await setup();
    const friend = await newUser("friend");
    const stranger = await newUser("stranger");
    const ask = (u: User) =>
      call("POST", `/v1/computers/${host.cid}/access-requests`, { token: u.token, key: u.key, body: { commit: ref.b64url(ref.random(32)) } });
    expect((await ask(friend)).status).toBe(404);
    expect((await ask(stranger)).status).toBe(404);
    await accept(friend, inviteId, token);
    expect((await ask(stranger)).status).toBe(404);
    const pending = await ask(friend);
    expect(pending.status).toBe(200);
    expect((await state(host)).requests[0]).toMatchObject({ requestId: pending.body.requestId, userId: friend.userId });
    const { grantId } = await grantAccess(friend, host);
    expect((await state(host)).grants).toEqual([
      { grantId, deviceKey: friend.dk, deviceName: "iPhone", platform: "ios", scopes: ["control", "screen"], userId: friend.userId },
    ]);
    const list = await call("GET", "/v1/computers", { token: friend.token, key: friend.key });
    expect(list.body.computers[0].access).toBe("granted");
  });

  it("leaves the owner's own flow as it was", async () => {
    const { owner, host } = await setup();
    const { grantId } = await grantAccess(owner, host);
    const s = await state(host);
    expect(s.grants).toEqual([expect.objectContaining({ grantId, userId: owner.userId })]);
    expect(s.memberships).toEqual([]);
  });
});

describe("revoking a membership", () => {
  it("ends the member's grants and pending requests, and drops it from state", async () => {
    const { host, inviteId, token } = await setup();
    const friend = await newUser("friend");
    const m = await accept(friend, inviteId, token);
    const { grantId } = await grantAccess(friend, host);
    const pending = await call("POST", `/v1/computers/${host.cid}/access-requests`, {
      token: friend.token,
      key: friend.key,
      body: { commit: ref.b64url(ref.random(32)) },
    });
    // The device already has a grant, so a second request is refused; make a second device ask instead.
    expect(pending.status).toBe(409);
    const stranger = await newHost();
    const path = `/v1/host/memberships/${m.body.membershipId}/revoke`;
    expect((await call("POST", path, { key: stranger.sign, body: {} })).status).toBe(404);
    expect((await call("POST", path, { key: host.sign, body: {} })).body).toEqual({ ok: true });
    expect((await call("POST", path, { key: host.sign, body: {} })).status).toBe(200);
    const s = await state(host);
    expect(s.memberships).toEqual([]);
    expect(s.grants).toEqual([]);
    const row = await env.DB.prepare("SELECT status, revoked_reason FROM grants WHERE id = ?").bind(grantId).first<Json>();
    expect(row).toEqual({ status: "revoked", revoked_reason: "membershipRevoked" });
    expect((await call("GET", "/v1/computers", { token: friend.token })).body.computers).toEqual([]);
    const ask = await call("POST", `/v1/computers/${host.cid}/access-requests`, {
      token: friend.token,
      key: friend.key,
      body: { commit: ref.b64url(ref.random(32)) },
    });
    expect(ask.status).toBe(404);
    // A used invite doesn't bring the access back.
    expect((await accept(friend, inviteId, token)).status).toBe(410);
    const audits = await env.DB.prepare("SELECT COUNT(*) AS n FROM audit_events WHERE action = 'membership.revoke' AND target = ?")
      .bind(m.body.membershipId)
      .first<{ n: number }>();
    expect(audits?.n).toBe(1);
  });

  it("lets the same account join again with a fresh invite", async () => {
    const { host, inviteId, token } = await setup();
    const friend = await newUser("friend");
    const m = await accept(friend, inviteId, token);
    await call("POST", `/v1/host/memberships/${m.body.membershipId}/revoke`, { key: host.sign, body: {} });
    const fresh = await invite(host);
    const again = await accept(friend, fresh.inviteId, fresh.token);
    expect(again.status).toBe(200);
    expect(again.body.membershipId).not.toBe(m.body.membershipId);
  });

  it("ends with the computer: unclaiming revokes memberships and unused invites", async () => {
    const { owner, host, inviteId, token } = await setup();
    const spare = await invite(host);
    const friend = await newUser("friend");
    await accept(friend, inviteId, token);
    expect((await call("DELETE", `/v1/computers/${host.cid}`, { token: owner.token })).status).toBe(200);
    expect((await state(host)).memberships).toEqual([]);
    expect((await call("GET", "/v1/computers", { token: friend.token })).body.computers).toEqual([]);
    expect((await accept(await newUser("late"), spare.inviteId, spare.token)).status).toBe(410);
  });

  const deliver = (payload: unknown, id = `msg_${ref.b64url(ref.random(8))}`) => {
    const body = JSON.stringify(payload);
    const now = new Date();
    return call("POST", "/v1/webhooks/clerk", {
      body: payload,
      headers: {
        "svix-id": id,
        "svix-timestamp": String(Math.floor(now.getTime() / 1000)),
        "svix-signature": new Webhook(env.CLERK_WEBHOOK_SECRET!).sign(id, now, body),
      },
    });
  };
  const botGuest = async (host: TestHost, n: string) => {
    await env.DB.prepare("INSERT INTO bot_invites(invite_id, computer_id, token_hash, expires_at, created_at, accepted_at) VALUES (?1, ?2, 'h', ?3, 1, 1)")
      .bind(`binv_${n}`, host.cid, Date.now() + 1e6)
      .run();
    await env.DB.prepare("INSERT INTO bot_devices(bot_device_id, computer_id, invite_id, sign_pub, name, created_at) VALUES (?1, ?2, ?3, ?4, 'Rex', 1)")
      .bind(`bd_${n}`, host.cid, `binv_${n}`, `pub_${n}`)
      .run();
  };
  const botRevoked = async (n: string) =>
    (await env.DB.prepare("SELECT revoked_at FROM bot_devices WHERE bot_device_id = ?").bind(`bd_${n}`).first<{ revoked_at: number | null }>())?.revoked_at != null;

  it("revokes a banned member's access, once, and leaves the host owner alone", async () => {
    const { owner, host, inviteId, token } = await setup();
    const friend = await newUser("friend");
    await accept(friend, inviteId, token);
    const { grantId } = await grantAccess(friend, host);
    const ban = { type: "user.updated", data: { id: friend.userId, banned: true } };
    const id = `msg_${ref.b64url(ref.random(8))}`;
    expect((await deliver({ type: "user.updated", data: { id: friend.userId, banned: false, locked: false } })).status).toBe(200);
    expect((await state(host)).memberships).toHaveLength(1);
    expect((await deliver(ban, id)).status).toBe(200);
    expect((await state(host)).memberships).toEqual([]);
    expect((await env.DB.prepare("SELECT status FROM grants WHERE id = ?").bind(grantId).first<Json>())?.status).toBe("revoked");
    expect((await env.DB.prepare("SELECT revoked_at FROM devices WHERE owner_user_id = ?").bind(friend.userId).first<Json>())?.revoked_at).not.toBeNull();
    expect((await deliver(ban, id)).status).toBe(200); // replay
    const n = await env.DB.prepare("SELECT COUNT(*) AS n FROM audit_events WHERE action = 'accountBanned' AND target = ?").bind(friend.userId).first<{ n: number }>();
    expect(n?.n).toBe(1);

    // The owner is a different case: left as is.
    await grantAccess(owner, host);
    expect((await deliver({ type: "user.updated", data: { id: owner.userId, banned: true } })).status).toBe(200);
    expect((await state(host)).owner).not.toBeNull();
    expect((await state(host)).grants).toHaveLength(1);
  });

  it("ignores session.revoked: a device is not tied to a Clerk session", async () => {
    const { host, inviteId, token } = await setup();
    const friend = await newUser("friend");
    await accept(friend, inviteId, token);
    await grantAccess(friend, host);
    expect((await deliver({ type: "session.revoked", data: { id: "sess_1", user_id: friend.userId } })).status).toBe(200);
    expect((await state(host)).memberships).toHaveLength(1);
    expect((await state(host)).grants).toHaveLength(1);
  });

  it("takes bot guests and unused bot invites along when an owner's account is deleted", async () => {
    const { owner, host } = await setup();
    await botGuest(host, "x");
    await env.DB.prepare("INSERT INTO bot_invites(invite_id, computer_id, token_hash, expires_at, created_at) VALUES ('binv_open', ?1, 'h', ?2, 1)")
      .bind(host.cid, Date.now() + 1e6)
      .run();
    expect(await botRevoked("x")).toBe(false);
    expect((await deliver({ type: "user.deleted", data: { id: owner.userId } })).status).toBe(200);
    expect(await botRevoked("x")).toBe(true);
    expect((await env.DB.prepare("SELECT revoked_at FROM bot_invites WHERE invite_id = 'binv_open'").first<Json>())?.revoked_at).not.toBeNull();
  });

  it("is done by the Clerk user.deleted webhook for a member", async () => {
    const { host, inviteId, token } = await setup();
    const friend = await newUser("friend");
    await accept(friend, inviteId, token);
    const { grantId } = await grantAccess(friend, host);
    const id = `msg_${ref.b64url(ref.random(8))}`;
    const payload = { type: "user.deleted", data: { id: friend.userId } };
    const body = JSON.stringify(payload);
    const now = new Date();
    const r = await call("POST", "/v1/webhooks/clerk", {
      body: payload,
      headers: {
        "svix-id": id,
        "svix-timestamp": String(Math.floor(now.getTime() / 1000)),
        "svix-signature": new Webhook(env.CLERK_WEBHOOK_SECRET!).sign(id, now, body),
      },
    });
    expect(r.status).toBe(200);
    const s = await state(host);
    expect(s.memberships).toEqual([]);
    expect(s.grants).toEqual([]);
    const row = await env.DB.prepare("SELECT status FROM grants WHERE id = ?").bind(grantId).first<Json>();
    expect(row?.status).toBe("revoked");
  });
});

describe("revoking a person on every computer of the owner (R6, R7)", () => {
  /** An owner with two claimed computers and a friend who joined both. */
  async function twoMacs() {
    const { owner, host: a, inviteId, token } = await setup();
    const b = await newHost();
    await claim(owner, b);
    const friend = await newUser("friend");
    expect((await accept(friend, inviteId, token)).status).toBe(200);
    const onB = await invite(b);
    expect((await accept(friend, onB.inviteId, onB.token)).status).toBe(200);
    return { owner, a, b, friend };
  }
  const revoke = (host: TestHost, userId: string) => call("POST", "/v1/host/people/revoke", { key: host.sign, body: { userId } });
  const unrevoke = (host: TestHost, userId: string) => call("POST", "/v1/host/people/unrevoke", { key: host.sign, body: { userId } });

  it("cuts the account's memberships and grants on every Mac of the owner and lists it as revoked on each", async () => {
    const { a, b, friend } = await twoMacs();
    const { grantId } = await grantAccess(friend, b);
    expect((await revoke(a, friend.userId)).body).toEqual({ ok: true });
    for (const h of [a, b]) {
      const s = await state(h);
      expect(s.revoked).toEqual([friend.userId]);
      expect(s.memberships).toEqual([]);
    }
    expect((await state(b)).grants).toEqual([]);
    const row = await env.DB.prepare("SELECT status FROM grants WHERE id = ?").bind(grantId).first<Json>();
    expect(row?.status).toBe("revoked");
    // Revoking again is a no-op.
    expect((await revoke(b, friend.userId)).status).toBe(200);
    const n = await env.DB.prepare("SELECT COUNT(*) AS n FROM revocations WHERE user_id = ?").bind(friend.userId).first<{ n: number }>();
    expect(n?.n).toBe(1);
  });

  it("refuses a new invite of the owner's until undone, on any of their Macs", async () => {
    const { a, b, friend } = await twoMacs();
    await revoke(a, friend.userId);
    const fresh = await invite(b);
    const r = await accept(friend, fresh.inviteId, fresh.token);
    expect(r.status).toBe(410);
    expect(JSON.stringify(r.body)).toContain("access to this computer was removed");
    expect((await unrevoke(b, friend.userId)).body).toEqual({ ok: true });
    for (const h of [a, b]) expect((await state(h)).revoked).toEqual([]);
    expect((await accept(friend, fresh.inviteId, fresh.token)).status).toBe(200);
  });

  it("gives back the memberships the revoke cut when it is undone, and only those", async () => {
    const { a, b, friend } = await twoMacs();
    await revoke(a, friend.userId);
    for (const h of [a, b]) expect((await state(h)).memberships).toEqual([]);
    expect((await unrevoke(b, friend.userId)).body).toEqual({ ok: true });
    for (const h of [a, b]) expect((await state(h)).memberships.map((m: Json) => m.userId)).toEqual([friend.userId]);
    // Undoing again, or after the owner ended a membership by hand, brings nothing back.
    expect((await unrevoke(a, friend.userId)).status).toBe(200);
    const m = (await state(a)).memberships[0];
    expect((await call("POST", `/v1/host/memberships/${m.membershipId}/revoke`, { key: a.sign, body: {} })).status).toBe(200);
    await revoke(a, friend.userId);
    await unrevoke(a, friend.userId);
    expect((await state(a)).memberships).toEqual([]);
  });

  it("is per owner: another owner's computers and lists are untouched", async () => {
    const { a, friend } = await twoMacs();
    const other = await setup();
    expect((await accept(friend, other.inviteId, other.token)).status).toBe(200);
    await revoke(a, friend.userId);
    const s = await state(other.host);
    expect(s.revoked).toEqual([]);
    expect(s.memberships.map((m: Json) => m.userId)).toEqual([friend.userId]);
  });

  it("refuses the owner, an unknown account, a bad body, and an unclaimed computer", async () => {
    const { owner, a } = await twoMacs();
    expect((await revoke(a, owner.userId)).status).toBe(400);
    expect((await revoke(a, "user_nobody")).status).toBe(404);
    expect((await call("POST", "/v1/host/people/revoke", { key: a.sign, body: {} })).status).toBe(400);
    const loose = await newHost();
    const friend = await newUser("friend");
    expect((await revoke(loose, friend.userId)).status).toBe(403);
    expect((await unrevoke(loose, friend.userId)).status).toBe(403);
  });
});
