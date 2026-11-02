// The CLI installation guide at /install and the /install.sh redirect.

import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ORIGIN } from "./helpers";

const get = (path: string) => SELF.fetch(`${ORIGIN}${path}`, { redirect: "manual" });
const RAW = "https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh";

describe("GET /install", () => {
  it("serves the guide with the install command and a script-free CSP", async () => {
    const res = await get("/install");
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("text/html; charset=utf-8");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    const csp = res.headers.get("content-security-policy")!;
    expect(csp).toContain("default-src 'none'");
    expect(csp).not.toContain("script-src");
    const html = await res.text();
    expect(html).toContain("<title>Install Kith</title>");
    expect(html).toContain("curl -fsSL https://kithbot.app/install.sh | sh");
    expect(html).toContain(RAW);
    expect(html).toContain("~/.kith/cli/current");
    expect(html).toContain("kith update");
    expect(html).not.toMatch(/<script/i);
  });
});

describe("GET /install.sh", () => {
  it("redirects to the script on GitHub", async () => {
    const res = await get("/install.sh");
    expect(res.status).toBe(302);
    expect(res.headers.get("location")).toBe(RAW);
    expect(res.headers.get("cache-control")).toBe("no-store");
  });
});

describe("links to the guide", () => {
  it("the home and download pages link /install", async () => {
    expect(await (await get("/")).text()).toContain("kithbot.app/install");
    expect(await (await get("/download")).text()).toContain('href="/install"');
  });
});
