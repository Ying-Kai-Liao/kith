// The download page and its kithbot.app redirects to the latest GitHub release.

import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { DOWNLOAD_SCRIPT } from "../src/routes/download";
import { ORIGIN } from "./helpers";

const get = (path: string) => SELF.fetch(`${ORIGIN}${path}`, { redirect: "manual" });
const BASE = "https://github.com/Ying-Kai-Liao/kith/releases/latest/download/";

describe("GET /download/*", () => {
  const cases: [string, string][] = [
    ["/download/mac", "kith-macos.dmg"],
    ["/download/mac/zip", "kith-macos.zip"],
    ["/download/linux", "kith-linux-x64.AppImage"],
    ["/download/linux/appimage", "kith-linux-x64.AppImage"],
    ["/download/linux/deb", "kith-linux-x64.deb"],
    ["/download/linux/tar.gz", "kith-linux-x64.tar.gz"],
  ];
  for (const [path, asset] of cases) {
    it(`${path} redirects to ${asset}`, async () => {
      const res = await get(path);
      expect(res.status).toBe(302);
      expect(res.headers.get("location")).toBe(BASE + asset);
      expect(res.headers.get("cache-control")).toBe("no-store");
    });
  }

  it("404s an unknown download", async () => {
    for (const p of ["/download/windows", "/download/mac/dmg", "/download/constructor", "/download/__proto__"]) {
      expect((await get(p)).status, p).toBe(404);
    }
  });
});

describe("GET /download", () => {
  it("serves a locked-down page whose CSP hash matches its script", async () => {
    const res = await get("/download");
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("text/html; charset=utf-8");
    expect(res.headers.get("cache-control")).toBe("public, max-age=300");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    const csp = res.headers.get("content-security-policy")!;
    for (const d of ["default-src 'none'", "base-uri 'none'", "form-action 'none'", "frame-ancestors 'none'"]) expect(csp).toContain(d);
    const html = await res.text();
    expect(html).not.toMatch(/<script[^>]*\ssrc/i);
    const inline = /<script>([\s\S]*?)<\/script>/.exec(html)![1];
    expect(inline).toBe(DOWNLOAD_SCRIPT);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(inline)));
    expect(csp).toContain(`script-src 'sha256-${btoa(String.fromCharCode(...digest))}'`);
  });

  it("links every download on kithbot.app, and only github for all releases and the install script", async () => {
    const html = await (await get("/download")).text();
    for (const p of ["mac", "mac/zip", "linux", "linux/deb", "linux/tar.gz"]) expect(html).toContain(`href="/download/${p}"`);
    expect(html).toContain("curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh | sh");
    const hrefs = [...html.matchAll(/href="([^"]+)"/g)].map((m) => m[1]);
    for (const h of hrefs) {
      if (h.startsWith("/download/")) continue;
      expect(h).toBe("https://github.com/Ying-Kai-Liao/kith/releases/latest");
    }
    expect(html).not.toMatch(/(?:src|href)="https?:\/\/(?!github\.com|raw\.githubusercontent\.com)/);
    expect(html).toContain("Apple silicon and Intel");
  });
});

describe("GET /download styles", () => {
  it("lets [hidden] beat a.big, so only the visitor OS button shows", async () => {
    const html = await (await get("/download")).text();
    expect(html).toContain("[hidden]{display:none!important}");
  });
});
