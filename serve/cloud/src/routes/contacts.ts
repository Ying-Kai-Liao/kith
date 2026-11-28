// Person-to-person chats (phase 4a): a contact per pair of accounts and a sealed queue of messages, one row per
// recipient device. The cloud never sees a plaintext or holds a key: it checks who may send, that the sender signed the
// exact ciphertext and recipient set, that the contact is live at the epoch named, and it queues until every recipient
// has collected or 30 days pass. Design: designs/2026-11-24-app-to-design/phase4a-build.md §1-§3.
//
// Wire strings (§3): every text field enters a hash or signature as its UTF-8 bytes exactly as sent (a key is its
// base64url text, a Mac's key is `computer:<id>`); epk, wrap, rhash and the ct hash are raw bytes; an epoch is 8 bytes
// big-endian.

import { b64url, concat, fromB64url, randomId, sha256, utf8, verifyEd25519 } from "../auth";
import { ApiError, body, device, host, notify, str, user, type Ctx } from "./common";
import type { Env } from "../index";
import type {
  Contact,
  ContactChange,
  ContactDirectory,
  PcInboxItem,
  PcSendStatus,
  Responses,
} from "../contract.gen";

const KEEP_MS = 30 * 86_400_000;
const MAX_CT = 64 * 1024;
const MIN_CT = 16;
const MAX_RECIPIENTS = 32;
const MAX_QUEUED_ROWS = 5_000;
const MAX_QUEUED_BYTES = 64 * 1024 * 1024;
const SENDS_PER_MINUTE = 60;
const INBOX_PAGE = 100;
const ACK_MAX = 90;
/** base64 of a 64 KiB ct plus up to 32 wraps and the framing. The body helper's default (64 KiB) is too small for it. */
const MAX_SEND_BODY = 112 * 1024;

const CONTACT_ID = /^ct_[A-Za-z0-9_-]{16,60}$/;
const MSG_ID = /^pm_[A-Za-z0-9_-]{22}$/;
export const BOX_SIG_LABEL = "kith/pc-box/v1";
const SIG_LABEL = "kith/pc/v1";

interface ContactRow {
  contact_id: string;
  user_lo: string;
  user_hi: string;
  inviter_user_id: string;
  epoch: number;
  base: number;
  live: number;
  ended_by: string | null;
  ended_at: number | null;
  made_rev: number;
  pgen: number;
  op: string | null;
}

const other = (k: ContactRow, me: string) => (k.user_lo === me ? k.user_hi : k.user_lo);

const fromHex = (h: string): Uint8Array => Uint8Array.from(h.match(/../g) ?? [], (x) => parseInt(x, 16));
/** A BLOB column read as hex (D1 hands back blobs in more than one shape) and written out as base64url. */
const hexToB64 = (h: string) => b64url(fromHex(h));
const ab = (u: Uint8Array): ArrayBuffer => u.buffer.slice(u.byteOffset, u.byteOffset + u.byteLength) as ArrayBuffer;
const u64be = (n: number) => {
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, BigInt(n));
  return out;
};

/** Who is on the other end of a call: a device of an account, or an account's computer. */
interface Party {
  userId: string;
  /** What a message is signed with and a recipient row is keyed by: a device's sign key, or `computer:<id>` for a Mac. */
  recipientKey: string;
  /** The key a signature by this party verifies under (a device's or a computer's sign key). */
  signKey: string;
}

async function loadContact(c: Ctx, contactId: string, userId: string): Promise<ContactRow> {
  if (!CONTACT_ID.test(contactId)) throw new ApiError("badRequest", "Invalid contactId");
  const row = await c.env.DB.prepare("SELECT * FROM contacts WHERE contact_id = ? AND (user_lo = ?2 OR user_hi = ?2)")
    .bind(contactId, userId)
    .first<ContactRow>();
  if (!row) throw new ApiError("notFound");
  return row;
}

/** Every key a message of this contact is sealed to: both accounts' devices with a box key, and their claimed computers. */
export async function directoryOf(env: Env, k: ContactRow): Promise<ContactDirectory> {
  const devices = await env.DB.prepare(
    `SELECT owner_user_id, sign_pub, box_pub, box_sig, platform, name, created_at FROM devices
     WHERE owner_user_id IN (?1, ?2) AND revoked_at IS NULL AND box_pub IS NOT NULL AND box_sig IS NOT NULL ORDER BY sign_pub`,
  )
    .bind(k.user_lo, k.user_hi)
    .all<{ owner_user_id: string; sign_pub: string; box_pub: string; box_sig: string; platform: string; name: string; created_at: number }>();
  const computers = await env.DB.prepare(
    "SELECT id, owner_user_id, sign_pub, box_pub FROM computers WHERE owner_user_id IN (?1, ?2) AND status = 'active' ORDER BY id",
  )
    .bind(k.user_lo, k.user_hi)
    .all<{ id: string; owner_user_id: string; sign_pub: string; box_pub: string }>();
  return {
    devices: devices.results.map((d) => ({
      userId: d.owner_user_id,
      key: d.sign_pub,
      boxKey: d.box_pub,
      boxSig: d.box_sig,
      platform: d.platform,
      name: d.name,
      addedAt: d.created_at,
    })),
    computers: computers.results.map((m) => ({ userId: m.owner_user_id, computerId: m.id, signKey: m.sign_pub, boxKey: m.box_pub })),
  };
}

const directoryKeys = (d: ContactDirectory): Map<string, string> =>
  new Map([...d.devices.map((x) => [x.key, x.userId] as const), ...d.computers.map((x) => [`computer:${x.computerId}`, x.userId] as const)]);

// ---- GET /v1/contacts ----

const endedByView = (k: ContactRow, me: string): "me" | "them" | "system" | null => {
  if (k.live || !k.ended_by) return null;
  if (k.ended_by === "accountDeleted") return "system";
  // owner:, computer: and backfill all mean the inviter's side ended it.
  return k.inviter_user_id === me ? "me" : "them";
};

export async function listContacts(c: Ctx): Promise<Responses["listContacts"]> {
  const u = await user(c);
  await device(c, u.userId);
  const { results } = await c.env.DB.prepare(
    `SELECT k.*, a.name AS other_name, a.email AS other_email,
       EXISTS (SELECT 1 FROM revocations r WHERE r.owner_user_id = k.inviter_user_id AND r.user_id = a.user_id) AS has_row
     FROM contacts k
     JOIN accounts a ON a.user_id = CASE WHEN k.user_lo = ?1 THEN k.user_hi ELSE k.user_lo END
     WHERE k.user_lo = ?1 OR k.user_hi = ?1 ORDER BY k.created_at, k.contact_id`,
  )
    .bind(u.userId)
    .all<ContactRow & { other_name: string | null; other_email: string | null; has_row: number }>();
  const contacts: Contact[] = [];
  for (const k of results) {
    contacts.push({
      contactId: k.contact_id,
      other: { userId: other(k, u.userId), name: k.other_name, email: k.other_email },
      role: k.inviter_user_id === u.userId ? "inviter" : "invitee",
      epoch: k.epoch,
      base: k.base,
      live: k.live === 1,
      // Paused: live, with the owner's revocation row (the agents are cut, the chat is not).
      paused: k.live === 1 && k.has_row === 1,
      endedBy: endedByView(k, u.userId),
      endedAt: k.ended_at,
      ...(k.live ? { directory: await directoryOf(c.env, k) } : {}),
    });
  }
  return { contacts };
}

// ---- send (§2.1) ----

/** The hash every recipient can check: the sorted (key, epk, wrap) list. */
async function recipientsHash(rs: { key: string; epk: Uint8Array; wrap: Uint8Array }[]): Promise<Uint8Array> {
  const sorted = [...rs].sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  return sha256(concat(...sorted.flatMap((r) => [utf8(r.key), r.epk, r.wrap])));
}

function sigInput(contactId: string, epoch: number, msgId: string, senderKey: string, rhash: Uint8Array, ctHash: Uint8Array): Uint8Array {
  return concat(utf8(SIG_LABEL), utf8(contactId), u64be(epoch), utf8(msgId), utf8(senderKey), rhash, ctHash);
}

const sent = (status: PcSendStatus, k: ContactRow | null, msgId: string | null, directory?: ContactDirectory): Responses["sendPersonChat"] => ({
  status,
  msgId,
  epoch: k?.epoch ?? null,
  ...(directory ? { directory } : {}),
});

/** Tells a recipient computer's relay there is something to collect (not `cloud.changed`, which takes the Mac out of sync). */
async function notifyPc(env: Env, computerId: string): Promise<void> {
  try {
    const res = await env.RELAY.get(env.RELAY.idFromName(computerId)).fetch("https://do/internal/pc", {
      method: "POST",
      headers: { "X-Kith-Internal": "1" },
    });
    if (!res.ok) console.error("relay pc notify failed", { computerId, status: res.status });
  } catch (e) {
    console.error("relay pc notify failed", { computerId, error: String(e) });
  }
}

/** A key that is live under an account outside this contact (or a computer nobody owns). */
async function isStranger(env: Env, k: ContactRow, recipientKey: string): Promise<boolean> {
  const row = recipientKey.startsWith("computer:")
    ? await env.DB.prepare("SELECT 1 FROM computers WHERE id = ?1 AND (owner_user_id IS NULL OR owner_user_id NOT IN (?2, ?3))")
        .bind(recipientKey.slice("computer:".length), k.user_lo, k.user_hi)
        .first()
    : await env.DB.prepare("SELECT 1 FROM devices WHERE sign_pub = ?1 AND revoked_at IS NULL AND owner_user_id NOT IN (?2, ?3)")
        .bind(recipientKey, k.user_lo, k.user_hi)
        .first();
  return !!row;
}

async function sendMessage(c: Ctx, contactId: string, from: Party): Promise<Responses["sendPersonChat"]> {
  const b = body(c, MAX_SEND_BODY);
  const msgId = str(b, "msgId", 40);
  if (!MSG_ID.test(msgId)) throw new ApiError("badRequest", "Invalid msgId");
  const epoch = b.epoch;
  if (!Number.isSafeInteger(epoch) || (epoch as number) < 1) throw new ApiError("badRequest", "Invalid epoch");
  const ct = fromB64url(b.ct);
  if (!ct || ct.length < MIN_CT) throw new ApiError("badRequest", "Invalid ct");
  if (ct.length > MAX_CT) throw new ApiError("tooLarge", "The message is too big");
  const sig = fromB64url(b.sig, 64);
  if (!sig) throw new ApiError("badRequest", "Invalid sig");
  if (!Array.isArray(b.recipients) || !b.recipients.length) throw new ApiError("badRequest", "Invalid recipients");
  if (b.recipients.length > MAX_RECIPIENTS) throw new ApiError("tooMany", "Too many recipients");
  const recipients = (b.recipients as Record<string, unknown>[]).map((r) => {
    const epk = fromB64url(r?.epk, 32);
    const wrap = fromB64url(r?.wrap, 48);
    if (typeof r?.key !== "string" || !r.key || r.key.length > 80 || !epk || !wrap) throw new ApiError("badRequest", "Invalid recipient");
    return { key: r.key, epk, wrap };
  });
  if (new Set(recipients.map((r) => r.key)).size !== recipients.length) throw new ApiError("badRequest", "A recipient is listed twice");

  const k = await loadContact(c, contactId, from.userId);

  // A resend of what is queued is answered as done, before any limit: the first copy already passed them.
  const have = await c.env.DB.prepare("SELECT contact_id, sender_key, hex(sig) AS sig FROM pc_messages WHERE msg_id = ?")
    .bind(msgId)
    .first<{ contact_id: string; sender_key: string; sig: string }>();
  if (have) {
    if (have.contact_id === contactId && have.sender_key === from.signKey && hexToB64(have.sig) === b.sig) return sent("duplicate", k, msgId);
    throw new ApiError("conflict", "msgId is taken");
  }
  if (!k.live) return sent("contactEnded", k, msgId);
  if (k.epoch !== epoch) return sent("staleEpoch", k, msgId);

  // Reads that may be stale by the time the batch runs; the batch re-checks the one that matters (live at this epoch).
  const directory = await directoryOf(c.env, k);
  const owners = directoryKeys(directory);
  if (owners.size > MAX_RECIPIENTS) throw new ApiError("tooMany", "Too many devices to seal to");
  const same = recipients.length === owners.size && recipients.every((r) => owners.has(r.key));
  if (!same) {
    // A live key of some other account is refused; a key that simply left the directory (a revoked device) or a device
    // that joined is the client's to re-seal for.
    for (const r of recipients) if (!owners.has(r.key) && (await isStranger(c.env, k, r.key))) throw new ApiError("badRequest", "Invalid recipient");
    return sent("recipientsChanged", k, msgId, directory);
  }
  if (owners.size === 0) throw new ApiError("badRequest", "Nobody to send to");

  const rhash = await recipientsHash(recipients);
  const signer = fromB64url(from.signKey, 32);
  if (!signer || !(await verifyEd25519(signer, sig, sigInput(contactId, epoch as number, msgId, from.signKey, rhash, await sha256(ct))))) {
    throw new ApiError("badRequest", "The message signature doesn't verify");
  }

  const recent = await c.env.DB.prepare("SELECT COUNT(*) AS n FROM pc_messages WHERE sender_key = ? AND created_at > ?")
    .bind(from.signKey, c.now - 60_000)
    .first<{ n: number }>();
  if ((recent?.n ?? 0) >= SENDS_PER_MINUTE) throw new ApiError("rateLimited");
  const used = await c.env.DB.prepare(
    `SELECT (SELECT COUNT(*) FROM pc_recipients r JOIN pc_messages m ON m.msg_id = r.msg_id WHERE m.sender_user_id = ?1) AS n,
            (SELECT COALESCE(SUM(length(ct)), 0) FROM pc_messages WHERE sender_user_id = ?1) AS bytes`,
  )
    .bind(from.userId)
    .first<{ n: number; bytes: number }>();
  if ((used?.n ?? 0) + recipients.length > MAX_QUEUED_ROWS || (used?.bytes ?? 0) + ct.length > MAX_QUEUED_BYTES) {
    throw new ApiError("full", "Too many messages are waiting; try again once they've been collected");
  }

  // One transaction: the live-at-this-epoch check and every row go in together or not at all, so an end that lands
  // first leaves nothing queued, and one that lands after drops what this queued for the other side.
  const stmts = [
    c.env.DB.prepare(
      `INSERT INTO pc_messages(msg_id, contact_id, epoch, sender_user_id, sender_key, ct, sig, rhash, created_at, expires_at)
       SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10
       WHERE EXISTS (SELECT 1 FROM contacts WHERE contact_id = ?2 AND live = 1 AND epoch = ?3)
       ON CONFLICT(msg_id) DO NOTHING`,
    ).bind(msgId, contactId, epoch, from.userId, from.signKey, ab(ct), ab(sig), ab(rhash), c.now, c.now + KEEP_MS),
    ...recipients.map((r) => {
      const isComputer = r.key.startsWith("computer:");
      // The device or computer must still be live: a revoke or unclaim that landed since the directory read wins.
      const guard = isComputer
        ? "EXISTS (SELECT 1 FROM computers WHERE id = substr(?2, 10) AND owner_user_id = ?3 AND status = 'active')"
        : "EXISTS (SELECT 1 FROM devices WHERE sign_pub = ?2 AND owner_user_id = ?3 AND revoked_at IS NULL)";
      return c.env.DB.prepare(
        `INSERT INTO pc_recipients(msg_id, recipient_key, recipient_user_id, epk, wrap)
         SELECT ?1, ?2, ?3, ?4, ?5
         WHERE EXISTS (SELECT 1 FROM pc_messages WHERE msg_id = ?1 AND epoch = ?6 AND sender_key = ?7 AND created_at = ?8) AND ${guard}
         ON CONFLICT(msg_id, recipient_key) DO NOTHING`,
      ).bind(msgId, r.key, owners.get(r.key), ab(r.epk), ab(r.wrap), epoch, from.signKey, c.now);
    }),
  ];
  const [first] = await c.env.DB.batch(stmts);
  if (first!.meta.changes === 1) {
    await Promise.all(
      directory.computers.filter((m) => m.signKey !== from.signKey).map((m) => notifyPc(c.env, m.computerId)),
    );
    return sent("ok", k, msgId);
  }
  // Nothing was queued: find out why from the rows as they are now.
  const now = await c.env.DB.prepare("SELECT contact_id, sender_key, hex(sig) AS sig FROM pc_messages WHERE msg_id = ?")
    .bind(msgId)
    .first<{ contact_id: string; sender_key: string; sig: string }>();
  if (now) {
    if (now.contact_id === contactId && now.sender_key === from.signKey && hexToB64(now.sig) === b.sig) return sent("duplicate", k, msgId);
    throw new ApiError("conflict", "msgId is taken");
  }
  const latest = await c.env.DB.prepare("SELECT * FROM contacts WHERE contact_id = ?").bind(contactId).first<ContactRow>();
  if (latest && !latest.live) return sent("contactEnded", latest, msgId);
  if (latest && latest.epoch !== epoch) return sent("staleEpoch", latest, msgId);
  throw new ApiError("conflict");
}

export async function sendPersonChat(c: Ctx, [contactId]: string[]): Promise<Responses["sendPersonChat"]> {
  const u = await user(c);
  const dev = await device(c, u.userId);
  return sendMessage(c, contactId!, { userId: u.userId, recipientKey: dev.dk, signKey: dev.dk });
}

export async function hostSendPersonChat(c: Ctx, [contactId]: string[]): Promise<Responses["hostSendPersonChat"]> {
  const comp = await host(c);
  if (!comp.owner_user_id) throw new ApiError("forbidden", "Sign in on the computer to chat");
  return sendMessage(c, contactId!, { userId: comp.owner_user_id, recipientKey: `computer:${comp.id}`, signKey: comp.sign_pub });
}

// ---- inbox and ack ----

async function inbox(c: Ctx, to: Party): Promise<Responses["pcInbox"]> {
  let after = { at: -1, id: "" };
  const cursor = c.url.searchParams.get("after");
  if (cursor) {
    const m = /^(\d{1,16})\.(pm_[A-Za-z0-9_-]{22})$/.exec(cursor);
    if (!m) throw new ApiError("badRequest", "Invalid after");
    after = { at: Number(m[1]), id: m[2]! };
  }
  const { results } = await c.env.DB.prepare(
    `SELECT m.msg_id, m.contact_id, m.epoch, m.sender_user_id, m.sender_key, m.created_at, hex(m.ct) AS ct, hex(m.sig) AS sig,
            hex(m.rhash) AS rhash, hex(r.epk) AS epk, hex(r.wrap) AS wrap
     FROM pc_recipients r JOIN pc_messages m ON m.msg_id = r.msg_id
     WHERE r.recipient_key = ?1 AND r.recipient_user_id = ?2 AND (m.created_at > ?3 OR (m.created_at = ?3 AND m.msg_id > ?4))
     ORDER BY m.created_at, m.msg_id LIMIT ?5`,
  )
    .bind(to.recipientKey, to.userId, after.at, after.id, INBOX_PAGE + 1)
    .all<{
      msg_id: string;
      contact_id: string;
      epoch: number;
      sender_user_id: string;
      sender_key: string;
      created_at: number;
      ct: string;
      sig: string;
      rhash: string;
      epk: string;
      wrap: string;
    }>();
  const page = results.slice(0, INBOX_PAGE);
  const messages: PcInboxItem[] = page.map((r) => ({
    msgId: r.msg_id,
    contactId: r.contact_id,
    epoch: r.epoch,
    senderUserId: r.sender_user_id,
    senderKey: r.sender_key,
    ct: hexToB64(r.ct),
    sig: hexToB64(r.sig),
    epk: hexToB64(r.epk),
    wrap: hexToB64(r.wrap),
    recipientsHash: hexToB64(r.rhash),
    createdAt: r.created_at,
  }));
  const last = page[page.length - 1];
  return { messages, next: results.length > INBOX_PAGE && last ? `${last.created_at}.${last.msg_id}` : null };
}

/** Deletes this recipient's rows, then the messages that have none left: one batch, so a message is never left half-owned. */
async function ack(c: Ctx, from: Party): Promise<Responses["pcAck"]> {
  const ids = body(c).msgIds;
  if (!Array.isArray(ids) || !ids.length || ids.length > ACK_MAX || ids.some((i) => typeof i !== "string" || !MSG_ID.test(i))) {
    throw new ApiError("badRequest", "Invalid msgIds");
  }
  const marks = ids.map(() => "?").join(",");
  await c.env.DB.batch([
    c.env.DB.prepare(`DELETE FROM pc_recipients WHERE recipient_key = ? AND recipient_user_id = ? AND msg_id IN (${marks})`).bind(
      from.recipientKey,
      from.userId,
      ...ids,
    ),
    c.env.DB.prepare(
      `DELETE FROM pc_messages WHERE msg_id IN (${marks}) AND NOT EXISTS (SELECT 1 FROM pc_recipients r WHERE r.msg_id = pc_messages.msg_id)`,
    ).bind(...ids),
  ]);
  return { ok: true };
}

export async function pcInbox(c: Ctx): Promise<Responses["pcInbox"]> {
  const u = await user(c);
  const dev = await device(c, u.userId);
  return inbox(c, { userId: u.userId, recipientKey: dev.dk, signKey: dev.dk });
}

export async function pcAck(c: Ctx): Promise<Responses["pcAck"]> {
  const u = await user(c);
  const dev = await device(c, u.userId);
  return ack(c, { userId: u.userId, recipientKey: dev.dk, signKey: dev.dk });
}

function computerParty(comp: { id: string; sign_pub: string; owner_user_id: string | null }): Party {
  if (!comp.owner_user_id) throw new ApiError("forbidden", "Sign in on the computer to chat");
  return { userId: comp.owner_user_id, recipientKey: `computer:${comp.id}`, signKey: comp.sign_pub };
}

export async function hostPcInbox(c: Ctx): Promise<Responses["hostPcInbox"]> {
  return inbox(c, computerParty(await host(c)));
}

export async function hostPcAck(c: Ctx): Promise<Responses["hostPcAck"]> {
  return ack(c, computerParty(await host(c)));
}

// ---- end and undo (§2.2, §2.3) ----

/** Every computer the account owns, for a change that holds on all of them. */
async function ownerComputerIds(env: Env, ownerUserId: string): Promise<string[]> {
  const rows = await env.DB.prepare("SELECT id FROM computers WHERE owner_user_id = ?").bind(ownerUserId).all<{ id: string }>();
  return rows.results.map((r) => r.id);
}

export interface ContactOutcome {
  status: ContactChange;
  epoch: number;
  live: boolean;
  /** The pause generation after the call (the one a stale answer names). */
  pgen: number;
}

/** The contact of an owner with another account, if there is one. */
export async function contactBetween(env: Env, a: string, b: string): Promise<ContactRow | null> {
  const [lo, hi] = a < b ? [a, b] : [b, a];
  return env.DB.prepare("SELECT * FROM contacts WHERE user_lo = ? AND user_hi = ?").bind(lo, hi).first<ContactRow>();
}

const reload = async (env: Env, contactId: string): Promise<ContactRow> => {
  const row = await env.DB.prepare("SELECT * FROM contacts WHERE contact_id = ?").bind(contactId).first<ContactRow>();
  if (!row) throw new ApiError("notFound");
  return row;
};

/** The devices whose grants a call just cut (stamped `now`, reason `membershipRevoked`) lose their sockets on every computer of the owner. */
async function tellOwnerComputers(c: Ctx, owner: string, them: string, cut: boolean): Promise<void> {
  const computers = await ownerComputerIds(c.env, owner);
  const { results } = cut
    ? await c.env.DB.prepare(
        `SELECT g.computer_id, g.id, d.sign_pub FROM grants g JOIN devices d ON d.id = g.device_id
         WHERE g.status = 'revoked' AND g.revoked_at = ?1 AND g.revoked_reason = 'membershipRevoked' AND d.owner_user_id = ?2
           AND g.computer_id IN (SELECT id FROM computers WHERE owner_user_id = ?3)`,
      )
        .bind(c.now, them, owner)
        .all<{ computer_id: string; id: string; sign_pub: string }>()
    : { results: [] as { computer_id: string; id: string; sign_pub: string }[] };
  await Promise.all(
    computers.map((id) =>
      notify(
        c.env,
        id,
        results.filter((g) => g.computer_id === id).map((g) => ({ dk: g.sign_pub, grantId: g.id })),
      ),
    ),
  );
}

/**
 * Ends a contact at the epoch the caller named, for its inviter (a Remove). One batch writes the end, the revocation row
 * (C-R) and every cut the revoke-person route makes (memberships and grants on the inviter's computers, pending access
 * requests), and drops the other account's queued copies; each statement after the first is guarded on the contact being
 * ended by this very call, so a race lost to another end or to an undo does nothing. The remover's own recipient rows
 * stay: their devices still collect the history. `made_rev` records whether this call made the revocation row (she was
 * not paused), so the undo knows whether to take it away again (§2.6).
 */
export async function endContact(c: Ctx, k: ContactRow, epoch: number, endedBy: string): Promise<ContactOutcome> {
  const owner = k.inviter_user_id;
  const them = other(k, owner);
  // The opening UPDATE writes a token of this call; every later statement checks it, so a call that lost a race (even one
  // stamped in the same millisecond, which `Date.now()` can't tell apart) changes nothing.
  const op = randomId("op_");
  const guard = "EXISTS (SELECT 1 FROM contacts WHERE contact_id = ?1 AND op = ?7)";
  const ownersComputers = "(SELECT id FROM computers WHERE owner_user_id = ?4)";
  const P = [k.contact_id, epoch, c.now, owner, them, endedBy, op];
  const run = (sql: string, _n?: number) => c.env.DB.prepare(sql).bind(...P);
  const noRow = "NOT EXISTS (SELECT 1 FROM revocations WHERE owner_user_id = ?4 AND user_id = ?5)";
  const [ended] = await c.env.DB.batch([
    // The revocation row is read here, before this batch's own insert below: made_rev is 1 only when the remove makes it.
    run(
      `UPDATE contacts SET live = 0, ended_by = ?6, ended_at = ?3, updated_at = ?3, op = ?7,
         made_rev = CASE WHEN ${noRow} THEN 1 ELSE 0 END, pgen = pgen + CASE WHEN ${noRow} THEN 1 ELSE 0 END
       WHERE contact_id = ?1 AND live = 1 AND epoch = ?2`,
      6,
    ),
    run(`INSERT OR IGNORE INTO revocations(owner_user_id, user_id, revoked_at) SELECT ?4, ?5, ?3 WHERE ${guard}`, 5),
    // Grants first, stamped like the host route's, so the undo SQL finds them under reason `membershipRevoked`.
    run(
      `UPDATE grants SET status = 'revoked', revoked_at = ?3, revoked_reason = 'membershipRevoked'
       WHERE status = 'active' AND device_id IN (SELECT id FROM devices WHERE owner_user_id = ?5)
         AND computer_id IN ${ownersComputers} AND ${guard}`,
      5,
    ),
    run(
      `UPDATE memberships SET revoked_at = ?3 WHERE user_id = ?5 AND revoked_at IS NULL AND computer_id IN ${ownersComputers} AND ${guard}`,
      5,
    ),
    run(
      `UPDATE access_requests SET status = 'cancelled', decided_at = ?3
       WHERE user_id = ?5 AND status = 'pending' AND computer_id IN ${ownersComputers} AND ${guard}`,
      5,
    ),
    run(
      `DELETE FROM pc_recipients WHERE recipient_user_id = ?5 AND msg_id IN (SELECT msg_id FROM pc_messages WHERE contact_id = ?1) AND ${guard}`,
      5,
    ),
    run(
      `DELETE FROM pc_messages WHERE contact_id = ?1 AND NOT EXISTS (SELECT 1 FROM pc_recipients r WHERE r.msg_id = pc_messages.msg_id) AND ${guard}`,
      3,
    ),
    run(
      `INSERT INTO audit_events(actor, action, target, result, created_at) SELECT ?6, 'contact.end', ?5, 'ok', ?3 WHERE ${guard}`,
      6,
    ),
  ]);
  if (ended!.meta.changes === 1) {
    await tellOwnerComputers(c, owner, them, true);
    return { status: "ok", epoch, live: false, pgen: (await reload(c.env, k.contact_id)).pgen };
  }
  const now = await reload(c.env, k.contact_id);
  if (now.epoch === epoch && !now.live) {
    // Already over at this epoch: the answer is the same, and the Macs are told again in case the first call's notice was lost.
    await tellOwnerComputers(c, owner, them, false);
    return { status: "alreadyEnded", epoch, live: false, pgen: now.pgen };
  }
  if (now.epoch !== epoch) return { status: "staleEpoch", epoch: now.epoch, live: now.live === 1, pgen: now.pgen };
  throw new ApiError("conflict");
}

/**
 * Brings an ended contact back for its inviter at a new epoch (base unchanged). Named: the ended epoch. Only when the
 * remove made the revocation row (`made_rev`) does the undo do what the host's undo does (memberships and grants this
 * contact's revoke cut come back, the row goes); a person who was paused before the remove stays paused (§2.6).
 */
export async function undoContact(c: Ctx, k: ContactRow, epoch: number, actor: string): Promise<ContactOutcome> {
  const owner = k.inviter_user_id;
  const them = other(k, owner);
  const revoked = await c.env.DB.prepare("SELECT revoked_at FROM revocations WHERE owner_user_id = ? AND user_id = ?")
    .bind(owner, them)
    .first<{ revoked_at: number }>();
  const op = randomId("op_");
  const guard = "EXISTS (SELECT 1 FROM contacts WHERE contact_id = ?1 AND op = ?8 AND made_rev = 1)";
  const ownersComputers = "(SELECT id FROM computers WHERE owner_user_id = ?4)";
  const P = [k.contact_id, epoch, c.now, owner, them, revoked?.revoked_at ?? 0, actor, op];
  const run = (sql: string, _n?: number) => c.env.DB.prepare(sql).bind(...P);
  const [back] = await c.env.DB.batch([
    run(
      `UPDATE contacts SET live = 1, epoch = epoch + 1, ended_by = NULL, ended_at = NULL, updated_at = ?3, op = ?8, pgen = pgen + made_rev
       WHERE contact_id = ?1 AND live = 0 AND epoch = ?2 AND inviter_user_id = ?4 AND COALESCE(ended_by, '') != 'accountDeleted'
         AND EXISTS (SELECT 1 FROM accounts WHERE user_id = ?5 AND status = 'active')`,
      8,
    ),
    // The memberships this revoke cut (stamped with its own time) are active again; one ended before keeps its end.
    run(
      `UPDATE memberships SET revoked_at = NULL WHERE user_id = ?5 AND revoked_at = ?6 AND computer_id IN ${ownersComputers} AND ${guard}`,
      6,
    ),
    // Their devices' grants the same revoke cut are granted again under new ids: the relay lifts its block on a device
    // only when the ACL lists it under another grant than the revoked one (§7.3).
    run(
      `INSERT INTO grants(id, computer_id, device_id, scopes, status, created_at)
       SELECT 'grt_' || lower(hex(randomblob(16))), g.computer_id, g.device_id, g.scopes, 'active', ?3
       FROM grants g JOIN devices d ON d.id = g.device_id
       WHERE g.status = 'revoked' AND g.revoked_reason = 'membershipRevoked' AND g.revoked_at = ?6
         AND d.owner_user_id = ?5 AND d.revoked_at IS NULL AND g.computer_id IN ${ownersComputers}
         AND NOT EXISTS (SELECT 1 FROM grants a WHERE a.computer_id = g.computer_id AND a.device_id = g.device_id AND a.status = 'active')
         AND ${guard}
       GROUP BY g.computer_id, g.device_id`,
      6,
    ),
    run(`DELETE FROM revocations WHERE owner_user_id = ?4 AND user_id = ?5 AND ${guard}`, 5),
    run(`INSERT INTO audit_events(actor, action, target, result, created_at) SELECT ?7, 'contact.undo', ?5, 'ok', ?3 WHERE EXISTS (SELECT 1 FROM contacts WHERE contact_id = ?1 AND op = ?8)`, 8),
  ]);
  if (back!.meta.changes === 1) {
    await tellOwnerComputers(c, owner, them, false);
    return { status: "ok", epoch: epoch + 1, live: true, pgen: (await reload(c.env, k.contact_id)).pgen };
  }
  const now = await reload(c.env, k.contact_id);
  // A repeat of an undo that stands; any other epoch means something happened since (a later end must not be undone by a late copy).
  if (now.live && now.epoch === epoch + 1) return { status: "alreadyLive", epoch: now.epoch, live: true, pgen: now.pgen };
  if (now.epoch !== epoch) return { status: "staleEpoch", epoch: now.epoch, live: now.live === 1, pgen: now.pgen };
  throw new ApiError("conflict", "This contact can't be brought back");
}

/**
 * Pause (§2.6): today's person revoke made one batch. The revocation row and every cut, guarded on the pause generation
 * bump that opens the batch; the contact stays live and the chat keeps working. Refused while the contact is removed,
 * and `stalePause` when the generation named is not the current one (a copy of an older pause or resume).
 */
export async function pauseContact(c: Ctx, k: ContactRow, pgen: number | null, actor: string): Promise<ContactOutcome> {
  const owner = k.inviter_user_id;
  const them = other(k, owner);
  const op = randomId("op_");
  const guard = "EXISTS (SELECT 1 FROM contacts WHERE contact_id = ?1 AND op = ?7)";
  const P = [k.contact_id, c.now, owner, them, actor, pgen, op];
  const run = (sql: string, _n?: number) => c.env.DB.prepare(sql).bind(...P);
  const mine = "(SELECT id FROM computers WHERE owner_user_id = ?3)";
  const [bumped] = await c.env.DB.batch([
    run("UPDATE contacts SET pgen = pgen + 1, updated_at = ?2, op = ?7 WHERE contact_id = ?1 AND live = 1 AND (?6 IS NULL OR pgen = ?6)", 7),
    run(`INSERT OR IGNORE INTO revocations(owner_user_id, user_id, revoked_at) SELECT ?3, ?4, ?2 WHERE ${guard}`, 4),
    run(
      `UPDATE grants SET status = 'revoked', revoked_at = ?2, revoked_reason = 'membershipRevoked'
       WHERE status = 'active' AND device_id IN (SELECT id FROM devices WHERE owner_user_id = ?4) AND computer_id IN ${mine} AND ${guard}`,
      4,
    ),
    run(`UPDATE memberships SET revoked_at = ?2 WHERE user_id = ?4 AND revoked_at IS NULL AND computer_id IN ${mine} AND ${guard}`, 4),
    run(
      `UPDATE access_requests SET status = 'cancelled', decided_at = ?2
       WHERE user_id = ?4 AND status = 'pending' AND computer_id IN ${mine} AND ${guard}`,
      4,
    ),
    run(`INSERT INTO audit_events(actor, action, target, result, created_at) SELECT ?5, 'person.revoke', ?4, 'ok', ?2 WHERE ${guard}`, 5),
  ]);
  if (bumped!.meta.changes === 1) {
    await tellOwnerComputers(c, owner, them, true);
    const now = await reload(c.env, k.contact_id);
    return { status: "ok", epoch: now.epoch, live: true, pgen: now.pgen };
  }
  const now = await reload(c.env, k.contact_id);
  if (!now.live) return { status: "removed", epoch: now.epoch, live: false, pgen: now.pgen };
  return { status: "stalePause", epoch: now.epoch, live: true, pgen: now.pgen };
}

/** Resume (§2.6): today's person undo, guarded on the same generation bump. Refused while the contact is removed (that is an undo). */
export async function resumeContact(c: Ctx, k: ContactRow, pgen: number | null, actor: string): Promise<ContactOutcome> {
  const owner = k.inviter_user_id;
  const them = other(k, owner);
  const revoked = await c.env.DB.prepare("SELECT revoked_at FROM revocations WHERE owner_user_id = ? AND user_id = ?")
    .bind(owner, them)
    .first<{ revoked_at: number }>();
  const op = randomId("op_");
  const guard = "EXISTS (SELECT 1 FROM contacts WHERE contact_id = ?1 AND op = ?8)";
  const P = [k.contact_id, c.now, owner, them, revoked?.revoked_at ?? 0, pgen, actor, op];
  const run = (sql: string, _n?: number) => c.env.DB.prepare(sql).bind(...P);
  const mine = "(SELECT id FROM computers WHERE owner_user_id = ?3)";
  const [bumped] = await c.env.DB.batch([
    run("UPDATE contacts SET pgen = pgen + 1, updated_at = ?2, op = ?8 WHERE contact_id = ?1 AND live = 1 AND (?6 IS NULL OR pgen = ?6)", 8),
    run(`UPDATE memberships SET revoked_at = NULL WHERE user_id = ?4 AND revoked_at = ?5 AND computer_id IN ${mine} AND ${guard}`, 5),
    run(
      `INSERT INTO grants(id, computer_id, device_id, scopes, status, created_at)
       SELECT 'grt_' || lower(hex(randomblob(16))), g.computer_id, g.device_id, g.scopes, 'active', ?2
       FROM grants g JOIN devices d ON d.id = g.device_id
       WHERE g.status = 'revoked' AND g.revoked_reason = 'membershipRevoked' AND g.revoked_at = ?5
         AND d.owner_user_id = ?4 AND d.revoked_at IS NULL AND g.computer_id IN ${mine}
         AND NOT EXISTS (SELECT 1 FROM grants a WHERE a.computer_id = g.computer_id AND a.device_id = g.device_id AND a.status = 'active')
         AND ${guard}
       GROUP BY g.computer_id, g.device_id`,
      5,
    ),
    run(`DELETE FROM revocations WHERE owner_user_id = ?3 AND user_id = ?4 AND ${guard}`, 4),
    run(`INSERT INTO audit_events(actor, action, target, result, created_at) SELECT ?7, 'person.unrevoke', ?4, 'ok', ?2 WHERE ${guard}`, 7),
  ]);
  if (bumped!.meta.changes === 1) {
    await tellOwnerComputers(c, owner, them, false);
    const now = await reload(c.env, k.contact_id);
    return { status: "ok", epoch: now.epoch, live: true, pgen: now.pgen };
  }
  const now = await reload(c.env, k.contact_id);
  if (!now.live) return { status: "removed", epoch: now.epoch, live: false, pgen: now.pgen };
  return { status: "stalePause", epoch: now.epoch, live: true, pgen: now.pgen };
}

function epochOf(c: Ctx): number {
  const epoch = body(c).epoch;
  if (!Number.isSafeInteger(epoch) || (epoch as number) < 1) throw new ApiError("badRequest", "Invalid epoch");
  return epoch as number;
}

export async function endContactRoute(c: Ctx, [contactId]: string[]): Promise<Responses["endContact"]> {
  const u = await user(c);
  await device(c, u.userId);
  const epoch = epochOf(c);
  const k = await loadContact(c, contactId!, u.userId);
  // v1: only the one whose invite made the contact may end it.
  if (k.inviter_user_id !== u.userId) throw new ApiError("forbidden", "Only the person who invited can remove this contact");
  return endContact(c, k, epoch, `owner:${u.userId}`);
}

export async function undoEndContactRoute(c: Ctx, [contactId]: string[]): Promise<Responses["undoEndContact"]> {
  const u = await user(c);
  const dev = await device(c, u.userId);
  const epoch = epochOf(c);
  // Undo widens access, and a web key lives with a browser login: a native device only.
  const platform = await c.env.DB.prepare("SELECT platform FROM devices WHERE id = ?").bind(dev.id).first<{ platform: string }>();
  if (platform?.platform === "web") throw new ApiError("forbidden", "Open Kith on your phone or computer to undo this");
  const k = await loadContact(c, contactId!, u.userId);
  if (k.inviter_user_id !== u.userId) throw new ApiError("forbidden", "Only the person who invited can bring this contact back");
  return undoContact(c, k, epoch, `owner:${u.userId}`);
}

/** The bytes a device's box key is signed over: the label, then the account and the key as text (§1.1). */
export const boxSigInput = (userId: string, boxKey: string): Uint8Array => utf8(BOX_SIG_LABEL + userId + boxKey);

/** True when `boxSig` is the device key's signature over its user and box key (§1.1). */
export async function boxSigValid(signKey: string, userId: string, boxKey: string, boxSig: string): Promise<boolean> {
  const pub = fromB64url(signKey, 32);
  const sig = fromB64url(boxSig, 64);
  return !!pub && !!sig && !!fromB64url(boxKey, 32) && verifyEd25519(pub, sig, boxSigInput(userId, boxKey));
}
