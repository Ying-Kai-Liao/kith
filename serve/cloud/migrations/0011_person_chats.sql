-- The sealed person-chat queue (phase 4a §1.2): one message body sealed once, one wrap of its key per recipient
-- device. The cloud never holds a key. A recipient row goes when that device acks it; a message goes when none is left,
-- or at 30 days.
CREATE TABLE pc_messages (
  msg_id         TEXT PRIMARY KEY,                -- pm_<b64url 16>, chosen by the sender: a resend is idempotent
  contact_id     TEXT NOT NULL REFERENCES contacts(contact_id),
  epoch          INTEGER NOT NULL,                -- the contact epoch it was composed under
  sender_user_id TEXT NOT NULL,
  sender_key     TEXT NOT NULL,                   -- device sign_pub, or a computer's sign_pub (the Mac sends too)
  ct             BLOB NOT NULL,                   -- the body, sealed once with a random content key
  sig            BLOB NOT NULL,                   -- 64 bytes, sender key, over the sig input
  rhash          BLOB NOT NULL,                   -- 32 bytes, hash of every recipient's (key, epk, wrap): in the sig input
  created_at     INTEGER NOT NULL,
  expires_at     INTEGER NOT NULL                 -- created_at + 30 days
);
CREATE INDEX pc_messages_contact ON pc_messages(contact_id, created_at);
CREATE INDEX pc_messages_expiry ON pc_messages(expires_at);
CREATE INDEX pc_messages_sender ON pc_messages(sender_user_id);
CREATE INDEX pc_messages_sender_key ON pc_messages(sender_key, created_at);

CREATE TABLE pc_recipients (                       -- one row per recipient device of either account
  msg_id            TEXT NOT NULL REFERENCES pc_messages(msg_id),
  recipient_key     TEXT NOT NULL,                -- device sign_pub, or 'computer:<id>'
  recipient_user_id TEXT NOT NULL,
  epk               BLOB NOT NULL,                -- 32
  wrap              BLOB NOT NULL,                -- 32-byte content key + 16-byte tag
  PRIMARY KEY (msg_id, recipient_key)
);
CREATE INDEX pc_recipients_key ON pc_recipients(recipient_key);
CREATE INDEX pc_recipients_user ON pc_recipients(recipient_user_id);
