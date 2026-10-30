-- Mac link (designs/2026-10-29-mac-link): one computer of an account asks another computer of the same account to let it
-- reach that computer's agents. The asking computer (from) brings a device key of its own and the account-device SAS
-- commit; the answering computer (to) sets its nonce and decides at the Mac. The cloud only offers the pair: a peer grant
-- is listed while both computers have the same owner, and unclaiming either one revokes it.
CREATE TABLE peer_requests (
  id               TEXT PRIMARY KEY,
  from_computer_id TEXT NOT NULL REFERENCES computers(id),
  to_computer_id   TEXT NOT NULL REFERENCES computers(id),
  device_key       TEXT NOT NULL,
  commit_hash      TEXT NOT NULL,
  host_nonce       TEXT,
  device_nonce     TEXT,
  status           TEXT NOT NULL DEFAULT 'pending',
  grant_id         TEXT,
  created_at       INTEGER NOT NULL,
  expires_at       INTEGER NOT NULL,
  decided_at       INTEGER
);
CREATE INDEX peer_requests_to ON peer_requests(to_computer_id, status);

CREATE TABLE peer_grants (
  id               TEXT PRIMARY KEY,
  from_computer_id TEXT NOT NULL REFERENCES computers(id),
  to_computer_id   TEXT NOT NULL REFERENCES computers(id),
  device_key       TEXT NOT NULL,
  status           TEXT NOT NULL DEFAULT 'active',
  created_at       INTEGER NOT NULL,
  revoked_at       INTEGER,
  revoked_reason   TEXT
);
CREATE UNIQUE INDEX peer_grants_active ON peer_grants(from_computer_id, to_computer_id) WHERE status = 'active';
