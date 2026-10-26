-- People an owner revoked (docs: designs/revoke-person, R6/R7/R9). One row per (owner, account): the revoke holds on
-- every computer the owner has, and undo deletes the row. The cloud keeps no access of its own here: a revoke cuts the
-- account's memberships on the owner's computers and refuses new ones; each Mac sets the revoked state in its own
-- kith-auth from this list (GET /v1/host/state `revoked`).
CREATE TABLE revocations (
  owner_user_id TEXT NOT NULL REFERENCES accounts(user_id),
  user_id       TEXT NOT NULL REFERENCES accounts(user_id),
  revoked_at    INTEGER NOT NULL,
  PRIMARY KEY (owner_user_id, user_id)
);
