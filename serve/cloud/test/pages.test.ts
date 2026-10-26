// The open-in-Kith page: security headers, a CSP hash that matches the inline script, no secrets, no external loads.

import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { INVITE_SCRIPT } from "../src/routes/pages";
import { ORIGIN } from "./helpers";

const get = (path: string) => SELF.fetch(`${ORIGIN}${path}`);

describe("GET /i/:cid/:iid", () => {
  it("serves a locked-down page", async () => {
    const res = await get("/i/cmp_abc-1/inv_XYZ_2");
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("text/html; charset=utf-8");
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(res.headers.get("referrer-policy")).toBe("no-referrer");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    const csp = res.headers.get("content-security-policy")!;
    for (const d of ["default-src 'none'", "base-uri 'none'", "form-action 'none'", "frame-ancestors 'none'"]) expect(csp).toContain(d);
    const html = await res.text();
    expect(html).not.toMatch(/<script[^>]*\ssrc/i);
    expect(html).not.toMatch(/https?:\/\//);
    // The CSP hash is the sha256 of exactly the inline script's text.
    const inline = /<script>([\s\S]*?)<\/script>/.exec(html)![1];
    expect(inline).toBe(INVITE_SCRIPT);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(inline)));
    expect(csp).toContain(`script-src 'sha256-${btoa(String.fromCharCode(...digest))}'`);
  });

  it("builds the kith:// link from the ids and the fragment token", async () => {
    const html = await (await get("/i/cmp_abc-1/inv_XYZ_2")).text();
    expect(html).toContain('data-c="cmp_abc-1"');
    expect(html).toContain('data-i="inv_XYZ_2"');
    expect(INVITE_SCRIPT).toContain('"kith://invite?c="');
    expect(INVITE_SCRIPT).toContain("location.hash");
    expect(INVITE_SCRIPT).toContain("encodeURIComponent(t)");
    expect(html).toContain("This invite link is incomplete. Ask for a new one.");
    expect(html).toContain("Don't have Kith yet?");
  });

  it("never sees a token: the fragment is not sent, and a query token is not echoed", async () => {
    const html = await (await get("/i/cmp_1/inv_1?t=SECRETSECRET")).text();
    expect(html).not.toContain("SECRETSECRET");
  });

  it("refuses odd ids and unknown paths", async () => {
    for (const p of ["/i/cmp<1/inv_1", "/i/cmp_1/inv%22x", `/i/${"a".repeat(65)}/inv_1`, "/i/cmp_1", "/i/cmp_1/inv_1/extra", "/i"]) {
      expect((await get(p)).status, p).toBeGreaterThanOrEqual(400);
    }
    expect((await get("/nope")).status).toBe(404);
  });
});
