-- Person-to-person chats (phase 4a): one contact per pair of accounts, with an epoch that moves at every re-live and a
-- base that moves only at a fresh start. See designs/2026-11-24-app-to-design/phase4a-build.md §1.1.
CREATE TABLE contacts (
  contact_id      TEXT PRIMARY KEY,              -- ct_<b64url 16>
  user_lo         TEXT NOT NULL REFERENCES accounts(user_id),   -- the pair, ordered: user_lo < user_hi
  user_hi         TEXT NOT NULL REFERENCES accounts(user_id),
  inviter_user_id TEXT NOT NULL REFERENCES accounts(user_id),   -- whose invite made it; v1: the one who may end it
  epoch           INTEGER NOT NULL DEFAULT 1,     -- +1 at every re-live (undo or re-add); never goes back
  base            INTEGER NOT NULL DEFAULT 1,     -- epoch of the last fresh start; an undo keeps it
  live            INTEGER NOT NULL CHECK (live IN (0,1)),
  ended_by        TEXT,                           -- owner:<uid> | computer:<id> | accountDeleted | backfill
  ended_at        INTEGER,
  made_rev        INTEGER NOT NULL DEFAULT 0,     -- the remove inserted the revocations row, so its undo deletes it (§2.6)
  pgen            INTEGER NOT NULL DEFAULT 0,     -- pause generation: +1 at every pause, resume, and a remove or undo that moves the row
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  UNIQUE (user_lo, user_hi),
  CHECK (user_lo < user_hi)
);
CREATE INDEX contacts_lo ON contacts(user_lo);
CREATE INDEX contacts_hi ON contacts(user_hi);

-- A device's X25519 key for person chats, signed by its own device key. NULL = an app from before 4a: not a recipient,
-- shows no person chats. Set once; a new box key is a new device.
ALTER TABLE devices ADD COLUMN box_pub TEXT;
ALTER TABLE devices ADD COLUMN box_sig TEXT;    -- Ed25519(sign_pub) over "kith/pc-box/v1" || user_id || box_pub
ALTER TABLE accounts ADD COLUMN name TEXT;      -- display name from the Clerk token, shown to the other side

-- Backfill 1: every active membership on a claimed computer is a live contact between the computer's owner (who
-- invited) and the member. One contact per pair, whichever computer or how many.
INSERT OR IGNORE INTO contacts(contact_id, user_lo, user_hi, inviter_user_id, epoch, base, live, created_at, updated_at)
SELECT 'ct_' || lower(hex(randomblob(16))), min(c.owner_user_id, m.user_id), max(c.owner_user_id, m.user_id),
       c.owner_user_id, 1, 1, 1, min(m.created_at), min(m.created_at)
FROM memberships m JOIN computers c ON c.id = m.computer_id
WHERE m.revoked_at IS NULL AND c.owner_user_id IS NOT NULL AND c.owner_user_id != m.user_id
GROUP BY min(c.owner_user_id, m.user_id), max(c.owner_user_id, m.user_id);

-- Backfill 2: every revocation whose pair has no contact yet is an ended contact (invariant C-R: ended implies a
-- revocations row; every row on main came from a Remove, so made_rev = 1 and an undo restores it all). The owner who revoked is the inviter.
INSERT OR IGNORE INTO contacts(contact_id, user_lo, user_hi, inviter_user_id, epoch, base, live, ended_by, ended_at, made_rev, created_at, updated_at)
SELECT 'ct_' || lower(hex(randomblob(16))), min(r.owner_user_id, r.user_id), max(r.owner_user_id, r.user_id),
       r.owner_user_id, 1, 1, 0, 'backfill', r.revoked_at, 1, r.revoked_at, r.revoked_at
FROM revocations r
WHERE r.owner_user_id != r.user_id
GROUP BY min(r.owner_user_id, r.user_id), max(r.owner_user_id, r.user_id);

-- A pair already live from step 1 but revoked (can't happen today: a revoke cuts every membership) is ended.
UPDATE contacts SET live = 0, ended_by = 'backfill', made_rev = 1, updated_at = (SELECT r.revoked_at FROM revocations r
    WHERE r.owner_user_id = contacts.inviter_user_id AND r.user_id IN (contacts.user_lo, contacts.user_hi) AND r.user_id != contacts.inviter_user_id),
  ended_at = (SELECT r.revoked_at FROM revocations r
    WHERE r.owner_user_id = contacts.inviter_user_id AND r.user_id IN (contacts.user_lo, contacts.user_hi) AND r.user_id != contacts.inviter_user_id)
WHERE live = 1 AND EXISTS (SELECT 1 FROM revocations r
    WHERE r.owner_user_id = contacts.inviter_user_id AND r.user_id IN (contacts.user_lo, contacts.user_hi) AND r.user_id != contacts.inviter_user_id);
