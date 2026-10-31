// Authentication: Clerk session JWTs (users) and Kith-Sig (host and device keys, spec §5).

import { verifyToken } from "@clerk/backend";
import type { Env } from "./index";

// ---- encoding ----

export const b64url = (bytes: Uint8Array): string => {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};

/** Strict base64url (no padding); `null` when malformed or not `len` bytes long. */
export function fromB64url(s: unknown, len?: number): Uint8Array | null {
  if (typeof s !== "string" || !/^[A-Za-z0-9_-]*$/.test(s) || s.length % 4 === 1) return null;
  const bytes = Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
  if (len !== undefined && bytes.length !== len) return null;
  // Reject non-canonical encodings (stray low bits in the last character).
  return b64url(bytes) === s ? bytes : null;
}

export const utf8 = (s: string) => new TextEncoder().encode(s);

export const concat = (...parts: Uint8Array[]): Uint8Array => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
};

export const sha256 = async (bytes: Uint8Array) => new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));

export const randomId = (prefix = "", n = 16) => prefix + b64url(crypto.getRandomValues(new Uint8Array(n)));

/** `computerId = b64url(SHA-256(hostSignPub)[0..16])` (§1). */
export const computerIdFor = async (signPub: Uint8Array) => b64url((await sha256(signPub)).subarray(0, 16));

export const COMPUTER_ID = /^[A-Za-z0-9_-]{22}$/;

// ---- Ed25519 (WebCrypto) ----

export async function verifyEd25519(pub: Uint8Array, sig: Uint8Array, msg: Uint8Array): Promise<boolean> {
  if (pub.length !== 32 || sig.length !== 64) return false;
  try {
    const key = await crypto.subtle.importKey("raw", pub, { name: "Ed25519" }, false, ["verify"]);
    return await crypto.subtle.verify({ name: "Ed25519" }, key, sig, msg);
  } catch {
    return false;
  }
}

// ---- Kith-Sig (§5) ----


export const SIG_WINDOW_MS = 300_000;
export const NONCE_TTL_MS = 600_000;

export interface SignedRequest {
  kid: string;
  ts: number;
  nonce: string;
}

/** The exact bytes a Kith-Sig signs. */
export async function sigCanonical(
  method: string,
  authority: string,
  pathAndQuery: string,
  ts: number,
  nonce: string,
  body: Uint8Array,
): Promise<string> {
  return [`kith-sig-v1`, method.toUpperCase(), authority.toLowerCase(), pathAndQuery, String(ts), nonce, b64url(await sha256(body))].join("\n");
}

/**
 * Checks the header's signature and time window. Replay (the `(kid, nonce)` pair) is the caller's job:
 * D1 `sig_nonces` for HTTP, the DO's `nonces` table for relay sockets. `null` = reject.
 */
export async function verifySig(req: Request, body: Uint8Array, now = Date.now(), headerValue?: string): Promise<SignedRequest | null> {
  const header = headerValue ?? req.headers.get("Kith-Sig");
  if (!header) return null;
  const f = new Map<string, string>();
  for (const part of header.split(",")) {
    const i = part.indexOf("=");
    if (i <= 0) return null;
    f.set(part.slice(0, i).trim(), part.slice(i + 1).trim());
  }
  const pub = fromB64url(f.get("kid"), 32);
  const sig = fromB64url(f.get("sig"), 64);
  const nonce = f.get("nonce") ?? "";
  const tsText = f.get("ts") ?? "";
  if (f.get("v") !== "1" || !pub || !sig || !fromB64url(nonce, 16) || !/^\d{1,16}$/.test(tsText)) return null;
  const ts = Number(tsText);
  if (Math.abs(now - ts) > SIG_WINDOW_MS) return null;
  const url = new URL(req.url);
  const canonical = await sigCanonical(req.method, url.host, url.pathname + url.search, ts, nonce, body);
  return (await verifyEd25519(pub, sig, utf8(canonical))) ? { kid: f.get("kid")!, ts, nonce } : null;
}

// ---- Clerk (§8.3) ----

export interface ClerkUser {
  userId: string;
  email: string | null;
  /** The Clerk session this token belongs to. */
  sid: string;
  /** The `Origin` the token was minted for; Clerk omits it when the Origin was empty. */
  azp: string | null;
}

/** `null` when the bearer token is missing or doesn't verify. */
export async function verifyClerk(req: Request, env: Env): Promise<ClerkUser | null> {
  const auth = req.headers.get("Authorization") ?? "";
  if (!auth.startsWith("Bearer ")) return null;
  return verifyClerkToken(auth.slice(7).trim(), env);
}

/** A bare session JWT (a browser socket carries it in a subprotocol, where there is no Authorization header). */
export async function verifyClerkToken(token: string, env: Env): Promise<ClerkUser | null> {
  const authorizedParties = (env.CLERK_AUTHORIZED_PARTIES ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  try {
    // With a secret key, @clerk/backend fetches the JWKS and caches it per isolate (rotation-aware).
    const claims = await verifyToken(token, {
      ...(env.CLERK_JWT_KEY ? { jwtKey: env.CLERK_JWT_KEY } : { secretKey: env.CLERK_SECRET_KEY }),
      ...(authorizedParties.length ? { authorizedParties } : {}),
    });
    if (claims.iss !== env.CLERK_ISSUER || typeof claims.sub !== "string" || !claims.sub || !claims.sid) return null;
    const email = (claims as { email?: unknown }).email;
    const azp = (claims as { azp?: unknown }).azp;
    return {
      userId: claims.sub,
      email: typeof email === "string" && email ? email : null,
      sid: claims.sid,
      azp: typeof azp === "string" && azp ? azp : null,
    };
  } catch {
    return null;
  }
}

/**
 * Asks Clerk whether a session is still live (GET /v1/sessions/{id}, status "active"). A token's signature and `exp`
 * only say it was valid a minute ago, not that the session hasn't ended since. Fails closed: no secret key, a network
 * error, a 4xx/5xx (a 429 on a dev instance included) or any other status all mean "not live".
 */
export async function clerkSessionActive(sid: string, env: Env): Promise<boolean> {
  if (!env.CLERK_SECRET_KEY || !/^[A-Za-z0-9_]{1,128}$/.test(sid)) return false;
  try {
    const res = await fetch(`https://api.clerk.com/v1/sessions/${sid}`, {
      headers: { Authorization: `Bearer ${env.CLERK_SECRET_KEY}` },
      signal: AbortSignal.timeout(5000),
    });
    if (!res.ok) return false;
    const body = (await res.json()) as { id?: unknown; status?: unknown };
    return body.id === sid && body.status === "active";
  } catch {
    return false;
  }
}

/** The origins allowed to host the web client (`WEB_ORIGINS`, comma list). Empty when unset. */
export const webOrigins = (env: Env): string[] =>
  (env.WEB_ORIGINS ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
