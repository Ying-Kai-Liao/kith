// Mac link pairing (designs/2026-10-29-mac-link): one computer asks another of the same account, the SAS runs through
// the cloud, the answering computer decides. The cloud only offers the pair and can only shorten it.

import { describe, expect, it } from "vitest";
import { call, clerkToken, type Json, type TestHost } from "./helpers";
import * as ref from "./ref";

let seq = 0;
const uid = (name: string) => `user_${name}_${++seq}_${ref.b64url(ref.random(4))}`;

async function newUser(name = "a") {
  const userId = uid(name);
  const token = await clerkToken(userId);
  const key = ref.signKey();
  const r = await call("POST", "/v1/devices", { token, key, body: { name: "iPhone", platform: "ios" } });
  expect(r.status).toBe(200);
  return { userId, token, key };
}
type User = Awaited<ReturnType<typeof newUser>>;

async function claim(user: User, host: TestHost, name: string) {
  const c = await call("POST", "/v1/claims", { token: user.token });
  const boxKey = ref.b64url(host.box.pub);
  const sig = ref.b64url(ref.sign(host.sign, ref.enc.encode(ref.claimCanonical(c.body.claimId, c.body.nonce, user.userId, host.cid, boxKey))));
  const r = await call("POST", `/v1/claims/${c.body.claimId}/complete`, {
    token: user.token,
    body: { computerId: host.cid, signKey: ref.b64url(host.sign.pub), boxKey, name, platform: "macos", device: "mac", version: "0.1.0", sig },
  });
  expect(r.status).toBe(200);
}

/** A registered computer named `name` (the name the cloud lists comes from registration). */
async function newHost(name = "Mac"): Promise<TestHost> {
  const sign = ref.signKey();
  const box = ref.boxKey();
  const r = await call("POST", "/v1/host/register", {
    key: sign,
    body: { boxKey: ref.b64url(box.pub), name, platform: "macos", device: "laptop", version: "0.1.0" },
  });
  expect(r.status).toBe(200);
  return { sign, box, cid: ref.computerId(sign.pub), ver: 0 };
}

const state = async (host: TestHost) => (await call("GET", "/v1/host/state", { key: host.sign })).body as Json;

/** An owner with two claimed computers: a laptop (asks) and a mini (answers). */
async function twoMacs() {
  const owner = await newUser("owner");
  const laptop = await newHost("laptop");
  const mini = await newHost("mini");
  await claim(owner, laptop, "laptop");
  await claim(owner, mini, "mini");
  return { owner, laptop, mini };
}

/** Runs the pairing up to the code on both sides; `decide` then approves or denies at the mini. */
async function pairUpTo(laptop: TestHost, mini: TestHost) {
  const dk = ref.signKey();
  const nD = ref.random(32);
  const req = await call("POST", "/v1/host/peer-requests", {
    key: laptop.sign,
    body: { computerId: mini.cid, deviceKey: ref.b64url(dk.pub), commit: ref.b64url(ref.sasCommit(dk.pub, nD)) },
  });
  expect(req.status).toBe(200);
  const id = req.body.requestId as string;
  const early = await call("POST", `/v1/host/peer-requests/${id}/reveal`, { key: laptop.sign, body: { nonce: ref.b64url(nD) } });
  expect(early.status).toBe(409);
  const nH = ref.random(32);
  expect((await call("POST", `/v1/host/peer-requests/${id}/nonce`, { key: mini.sign, body: { nonce: ref.b64url(nH) } })).status).toBe(200);
  const seen = await call("GET", `/v1/host/peer-requests/${id}`, { key: laptop.sign });
  expect(seen.body).toMatchObject({ status: "pending", computerId: mini.cid, hostNonce: ref.b64url(nH) });
  expect((await call("POST", `/v1/host/peer-requests/${id}/reveal`, { key: laptop.sign, body: { nonce: ref.b64url(nD) } })).status).toBe(200);
  const r = (await state(mini)).peerRequests.find((x: Json) => x.requestId === id);
  expect(r).toMatchObject({ computerId: laptop.cid, name: "laptop", deviceKey: ref.b64url(dk.pub) });
  expect(ref.b64url(ref.sasCommit(ref.unb64url(r.deviceKey), ref.unb64url(r.deviceNonce)))).toBe(r.commit);
  return { id, dk, code: ref.sasCode(mini.sign.pub, dk.pub, nD, nH) };
}

const decide = (mini: TestHost, id: string, decision: string) =>
  call("POST", `/v1/host/peer-requests/${id}/decision`, { key: mini.sign, body: { decision } });

describe("Mac link pairing", () => {
  it("offers the account's other computers, with their keys", async () => {
    const { laptop, mini } = await twoMacs();
    const r = await call("GET", "/v1/host/peer-computers", { key: laptop.sign });
    expect(r.body.computers).toEqual([{ computerId: mini.cid, name: "mini", platform: "macos", signKey: ref.b64url(mini.sign.pub) }]);
  });

  it("pairs: the grant is listed on both sides, apart from device grants", async () => {
    const { laptop, mini } = await twoMacs();
    const { id, dk } = await pairUpTo(laptop, mini);
    const d = await decide(mini, id, "approve");
    expect(d.body.status).toBe("approved");
    const grantId = d.body.grantId as string;
    expect((await call("GET", `/v1/host/peer-requests/${id}`, { key: laptop.sign })).body).toMatchObject({ status: "approved", grantId });
    const onMini = await state(mini);
    const onLaptop = await state(laptop);
    const peer = { grantId, fromComputerId: laptop.cid, toComputerId: mini.cid, deviceKey: ref.b64url(dk.pub) };
    expect(onMini.peers).toEqual([{ ...peer, name: "laptop" }]);
    expect(onLaptop.peers).toEqual([{ ...peer, name: "mini" }]);
    // An older host reads grants and requests only: the peer's key is never there.
    expect(onMini.grants.some((g: Json) => g.deviceKey === peer.deviceKey)).toBe(false);
    expect(onMini.requests).toEqual([]);
  });

  it("refuses a computer of another account, and a computer pairing with itself", async () => {
    const { laptop, mini } = await twoMacs();
    const other = await newUser("other");
    const theirs = await newHost();
    await claim(other, theirs, "theirs");
    const dk = ref.signKey();
    const body = (cid: string) => ({ computerId: cid, deviceKey: ref.b64url(dk.pub), commit: ref.b64url(ref.random(32)) });
    expect((await call("POST", "/v1/host/peer-requests", { key: laptop.sign, body: body(theirs.cid) })).status).toBe(404);
    expect((await call("POST", "/v1/host/peer-requests", { key: laptop.sign, body: body(laptop.cid) })).status).toBe(404);
    expect((await call("GET", "/v1/host/peer-computers", { key: theirs.sign })).body.computers).toEqual([]);
    // Only the asked computer decides, and only the asking one reads or reveals.
    const { id } = await pairUpTo(laptop, mini);
    expect((await decide(laptop, id, "approve")).status).toBe(404);
    expect((await call("GET", `/v1/host/peer-requests/${id}`, { key: mini.sign })).status).toBe(404);
  });

  it("a deny leaves nothing paired", async () => {
    const { laptop, mini } = await twoMacs();
    const { id } = await pairUpTo(laptop, mini);
    expect((await decide(mini, id, "deny")).body).toEqual({ status: "denied" });
    expect((await state(mini)).peers).toEqual([]);
    expect((await call("GET", `/v1/host/peer-requests/${id}`, { key: laptop.sign })).body.status).toBe("denied");
  });

  it("a new pairing ends the old one", async () => {
    const { laptop, mini } = await twoMacs();
    const first = await pairUpTo(laptop, mini);
    const g1 = (await decide(mini, first.id, "approve")).body.grantId;
    const second = await pairUpTo(laptop, mini);
    const g2 = (await decide(mini, second.id, "approve")).body.grantId;
    expect(g2).not.toBe(g1);
    expect((await state(mini)).peers.map((p: Json) => p.grantId)).toEqual([g2]);
  });

  it("either side ends it, and an unclaim of either ends it", async () => {
    const { owner, laptop, mini } = await twoMacs();
    const p = await pairUpTo(laptop, mini);
    const grantId = (await decide(mini, p.id, "approve")).body.grantId;
    expect((await call("POST", `/v1/host/peers/${grantId}/revoke`, { key: laptop.sign })).status).toBe(200);
    expect((await state(mini)).peers).toEqual([]);

    const again = await pairUpTo(laptop, mini);
    await decide(mini, again.id, "approve");
    expect((await state(laptop)).peers).toHaveLength(1);
    expect((await call("POST", "/v1/host/unclaim", { key: mini.sign })).status).toBe(200);
    expect((await state(laptop)).peers).toEqual([]);
    // Claimed again by the same owner, the old pairing does not come back: it needs a new code.
    await claim(owner, mini, "mini");
    expect((await state(laptop)).peers).toEqual([]);
    expect((await state(mini)).peers).toEqual([]);
  });

  it("a computer that changes owner is no one's peer", async () => {
    const { laptop, mini } = await twoMacs();
    const p = await pairUpTo(laptop, mini);
    await decide(mini, p.id, "approve");
    // The grant row stays active, but the cloud lists it only while both have one owner.
    const other = await newUser("other");
    await call("POST", "/v1/host/unclaim", { key: mini.sign });
    await claim(other, mini, "mini");
    expect((await state(mini)).peers).toEqual([]);
    expect((await state(laptop)).peers).toEqual([]);
  });

  it("can't be approved after the asker's account lost the computer", async () => {
    const { laptop, mini } = await twoMacs();
    const { id } = await pairUpTo(laptop, mini);
    await call("POST", "/v1/host/unclaim", { key: laptop.sign });
    expect((await decide(mini, id, "approve")).status).toBe(409);
    expect((await state(mini)).peers).toEqual([]);
  });
});
