/// <reference types="vite/client" />
// The cloud contract (serve/contract/cloud/contract.json) as the cloud's tests hold it to: the generated file is the
// contract's, today's answers (fixtures/) are what the cloud must send, and older and newer clouds' answers
// (fixtures/compat/, fixtures/unknown/) are what an app must still read. Every other test's JSON answers are checked
// against their route's type as well (KITH_CONTRACT_STRICT in vitest.config.ts).

import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { check, CONTRACT_SHA256, ERROR_STATUS, receive, ROUTES, SCHEMA } from "../src/contract.gen";
import { ApiError } from "../src/routes/common";
import { env } from "./helpers";

const contract = import.meta.glob<string>("../../contract/cloud/contract.json", { query: "?raw", import: "default", eager: true });
const fixtures = import.meta.glob<{ type: string; body: unknown }>("../../contract/cloud/fixtures/**/*.json", {
  import: "default",
  eager: true,
});
const named = (dir: "" | "compat/" | "unknown/") =>
  Object.entries(fixtures).filter(([path]) => path.replace(/^.*\/fixtures\//, "").replace(/[^/]+$/, "") === dir);

const hex = (b: ArrayBuffer) => [...new Uint8Array(b)].map((x) => x.toString(16).padStart(2, "0")).join("");

describe("cloud contract", () => {
  it("src/contract.gen.ts is made from contract.json as it is now (else: node serve/contract/gen.mjs)", async () => {
    const [raw] = Object.values(contract);
    expect(raw).toBeTypeOf("string");
    expect(hex(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(raw)))).toBe(CONTRACT_SHA256);
  });

  it("strict checking is on for these tests", () => {
    expect(env.KITH_CONTRACT_STRICT).toBe("1");
  });

  it("today's answers are what the cloud must send", () => {
    expect(named("").length).toBeGreaterThan(15);
    for (const [path, { type, body }] of named("")) expect(check(type, body, "producer"), path).toEqual([]);
  });

  it("every JSON route's answer type has a fixture", () => {
    const covered = new Set(named("").map(([, f]) => f.type));
    for (const r of Object.values(ROUTES)) if ("response" in r) expect(covered, r.path).toContain(r.response);
  });

  it("an older cloud's answers still read (W4), though the cloud no longer sends them", () => {
    expect(named("compat/").length).toBeGreaterThan(0);
    for (const [path, { type, body }] of named("compat/")) {
      expect(check(type, body, "consumer"), path).toEqual([]);
      expect(check(type, body, "producer"), path).not.toEqual([]);
    }
  });

  it("a newer cloud's words read as their enum's declared unknown (W3), and the cloud can't send them", () => {
    expect(named("unknown/").length).toBeGreaterThan(0);
    for (const [path, { type, body }] of named("unknown/")) {
      expect(check(type, body, "consumer"), path).toEqual([]);
      expect(check(type, body, "producer"), path).not.toEqual([]);
      // Every word the contract doesn't name is replaced by what its enum says it means.
      expect(receive(type as never, body), path).not.toEqual(body);
    }
    const [, list] = named("unknown/").find(([, f]) => f.type === "ComputerList")!;
    const [row] = receive("ComputerList", list.body).computers;
    expect([row.role, row.access]).toEqual(["member", "none"]);
    const [, status] = named("unknown/").find(([, f]) => f.type === "AccessStatus")!;
    expect(receive("AccessStatus", status.body).status).toBe("unknown");
  });

  it("every enum says what a word it doesn't know means, and never sends 'unknown'", () => {
    for (const [name, e] of Object.entries(SCHEMA.enums)) {
      expect(e.values, name).not.toContain("unknown");
      expect([...e.values, "unknown"], name).toContain(e.unknown);
    }
    const computer = { computerId: "c", name: "n", signKey: "k" };
    expect(check("CloudComputer", { ...computer, role: "unknown" }, "producer").join()).toContain('"unknown" is not a ComputerRole');
    expect(check("AccessStatus", { status: "unknown" }, "producer").join()).toContain('"unknown" is not a AccessState');
  });

  it("refuses the #328 word: a computer's access is never 'approved'", () => {
    const computer = { computerId: "c", name: "n", signKey: "k", access: "approved" };
    expect(check("CloudComputer", computer, "producer").join()).toContain('"approved" is not a ComputerAccess');
    expect(receive("CloudComputer", computer).access).toBe("none");
  });

  it("each error code has the contract's status", () => {
    for (const [code, status] of Object.entries(ERROR_STATUS)) expect(new ApiError(code as keyof typeof ERROR_STATUS).status).toBe(status);
  });

  it("a live answer is checked: GET /v1/health is the contract's Health", async () => {
    const res = await SELF.fetch("https://cloud.test/v1/health");
    expect(res.status).toBe(200);
    expect(check("Health", await res.json(), "producer")).toEqual([]);
  });
});
