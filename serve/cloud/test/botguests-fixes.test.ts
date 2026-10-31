// The follow-ups of #127's security review (docs/kith-bot-guests.md 3 and 5.4): the join limit runs before the signature,
// an unknown invite and a wrong token look alike, a revoke that lost a race says so, an old device's revoke can't block
// a newer one, names carry no control characters, and unclaiming a computer ends its bots.

import { describe, expect, it } from "vitest";
import { sha256 } from "@noble/hashes/sha2.js";
import { unclaim } from "../src/routes/common";
import { call, devicePath, env, hostSocket, newHost, socket, upgrade, type TestHost } from "./helpers";
import * as ref from "./ref";

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const dkOf = (k: ref.SignKey) => ref.b64url(k.pub);

function secrets() {
  const t = ref.b64url(ref.random(32));
  const proof = hex(sha256(ref.enc.encode(t)));
  return { proof, tokenHash: hex(sha256(ref.enc.encode(proof))) };
}

async function invite(host: TestHost) {
  const inviteId = `binv_${ref.b64url(ref.random(8))}`;
  const s = secrets();
  const r = await call("POST", "/v1/host/bot-invites", {
    key: host.sign,
    body: { inviteId, tokenHash: s.tokenHash, expiresAt: Date.now() + 24 * 3600_000 },
  });
  expect(r.status).toBe(200);
  return { inviteId, ...s };
}

const accept = (bot: ref.SignKey, inviteId: string, token: string, body: Record<string, unknown> = {}) =>
  call("POST", `/v1/bot-invites/${inviteId}/accept`, { key: bot, body: { token, name: "Rex", provider: "grok", ...body } });

const revokeDevice = (host: TestHost, id: string) => call("POST", `/v1/host/bot-devices/${id}/revoke`, { key: host.sign, body: {} });
const count = async (sql: string, ...binds: unknown[]) => (await env.DB.prepare(sql).bind(...binds).first<{ n: number }>())!.n;

// The limiter's windows are fixed and aligned to the wall clock (miniflare's RateLimiterObject: epoch = floor(now / period)).
// A burst that crosses a minute boundary gets 10 requests in one window and the 11th in a fresh one, which answers 401,
// so the burst has to fall inside one window: start it early in a window, and if it still crosses the edge, run it once more.
const WINDOW_MS = 60_000;
const EARLY_IN_WINDOW_MS = 5_000;
const windowOf = () => Math.floor(Date.now() / WINDOW_MS);
const msToNextWindow = () => WINDOW_MS - (Date.now() % WINDOW_MS);
const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

async function floodFrom(ip: string, inviteId: string, proof: string): Promise<number[] | null> {
  if (msToNextWindow() < EARLY_IN_WINDOW_MS) await pause(msToNextWindow() + 50);
  const headers = { "CF-Connecting-IP": ip };
  const window = windowOf();
  const statuses = [];
  for (let i = 0; i < 11; i++) {
    statuses.push((await call("POST", `/v1/bot-invites/${inviteId}/accept`, { headers, body: { token: proof, name: "Rex" } })).status);
  }
  return windowOf() === window ? statuses : null;
}

describe("bot join limiter", () => {
  it("counts a request before its signature is checked, so unsigned floods are limited too", async () => {
    const { inviteId, proof } = { inviteId: "binv_flood", proof: secrets().proof };
    const statuses =(await floodFrom("192.0.2.201", inviteId, proof)) ?? (await floodFrom("192.0.2.203", inviteId, proof));
    expect(statuses, "the burst crossed a window edge twice").not.toBeNull();
    expect(statuses!.slice(0, 10)).toEqual(Array(10).fill(401));
    expect(statuses![10]).toBe(429);
  }, 30_000);
});

describe("unknown invite and wrong token", () => {
  it("answer the same 403, so invite ids can't be probed", async () => {
    const host = await newHost();
    const { inviteId } = await invite(host);
    const bot = ref.signKey();
    const wrong = await accept(bot, inviteId, secrets().proof);
    const missing = await accept(bot, "binv_nothere", secrets().proof);
    expect(wrong.status).toBe(403);
    expect(missing.status).toBe(403);
    expect(missing.body.error).toEqual(wrong.body.error);
  });
});

describe("revoking a bot invite", () => {
  it("answers 409 when the bot accepted first, and audits only a revoke that happened", async () => {
    const host = await newHost();
    const { inviteId } = await invite(host);
    // Stands in for the accept landing between the read and the write: the UPDATE matches no row.
    await env.DB.exec(
      `CREATE TRIGGER race BEFORE UPDATE ON bot_invites WHEN NEW.revoked_at IS NOT NULL AND OLD.invite_id = '${inviteId}' BEGIN SELECT RAISE(IGNORE); END`,
    );
    try {
      const r = await call("POST", `/v1/host/bot-invites/${inviteId}/revoke`, { key: host.sign, body: {} });
      expect(r.status).toBe(409);
      expect(r.body.error.code).toBe("conflict");
      expect(await count("SELECT COUNT(*) AS n FROM audit_events WHERE action = 'botinvite.revoke' AND target = ?", inviteId)).toBe(0);
    } finally {
      await env.DB.exec("DROP TRIGGER race");
    }
    expect((await call("POST", `/v1/host/bot-invites/${inviteId}/revoke`, { key: host.sign, body: {} })).status).toBe(200);
  });
});

describe("re-sent device revoke", () => {
  it("doesn't block a newer device that holds the same key", async () => {
    const host = await newHost();
    const first = await invite(host);
    const bot = ref.signKey();
    const old = await accept(bot, first.inviteId, first.proof);
    expect((await revokeDevice(host, old.body.botDeviceId)).status).toBe(200);
    const second = await invite(host);
    const again = await accept(bot, second.inviteId, second.proof);
    expect(again.status).toBe(200);
    const hostWs = await hostSocket(host, [{ dk: dkOf(bot), grant: `bot:${again.body.botDeviceId}`, kind: "bot" }]);
    const sock = await socket(devicePath(host), bot);
    await hostWs.nextT("open");
    // The host retries the old revoke (a notify it thought was lost).
    expect((await revokeDevice(host, old.body.botDeviceId)).status).toBe(200);
    await sock.none((m) => m.t === "close", 200);
    expect(sock.close).toBeUndefined();
    expect((await upgrade(devicePath(host), bot)).status).toBe(101);
    // Revoking the live one still blocks.
    expect((await revokeDevice(host, again.body.botDeviceId)).status).toBe(200);
    expect(await sock.closed()).toBe(4003);
  });
});

describe("bot names", () => {
  it("refuse control characters, in the name and the provider", async () => {
    const host = await newHost();
    const { inviteId, proof } = await invite(host);
    const bot = ref.signKey();
    for (const name of ["Re\nx", "Re\u0000x", "Re\u001bx", "Re\u007fx", "Re\u0085x"]) {
      const r = await accept(bot, inviteId, proof, { name });
      expect(r.status, JSON.stringify(name)).toBe(400);
    }
    expect((await accept(bot, inviteId, proof, { provider: "gr\nok" })).status).toBe(400);
    // Nothing was burned, and ordinary Unicode is fine.
    expect((await accept(bot, inviteId, proof, { name: "Rex \u{1F916} é" })).status).toBe(200);
  });
});

describe("unclaiming a computer", () => {
  it("revokes its open bot invites and bot devices and closes their sockets", async () => {
    const host = await newHost();
    const joined = await invite(host);
    const spare = await invite(host);
    const bot = ref.signKey();
    const device = await accept(bot, joined.inviteId, joined.proof);
    const hostWs = await hostSocket(host, [{ dk: dkOf(bot), grant: `bot:${device.body.botDeviceId}`, kind: "bot" }]);
    const sock = await socket(devicePath(host), bot);
    await hostWs.nextT("open");

    await unclaim(env, host.cid, "test", Date.now());

    expect(await sock.closed()).toBe(4003);
    expect(await count("SELECT COUNT(*) AS n FROM bot_devices WHERE computer_id = ? AND revoked_at IS NULL", host.cid)).toBe(0);
    expect(await count("SELECT COUNT(*) AS n FROM bot_invites WHERE computer_id = ? AND accepted_at IS NULL AND revoked_at IS NULL", host.cid)).toBe(0);
    expect((await call("GET", "/v1/host/state", { key: host.sign })).body.botDevices).toEqual([]);
    const late = await accept(ref.signKey(), spare.inviteId, spare.proof);
    expect(late.status).toBe(410);
    expect((await upgrade(devicePath(host), bot)).status).toBe(403);
  });
});
