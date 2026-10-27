# Packaging and release secrets

- `homebrew/` the Homebrew formula template (see its README).
- `minisign.pub` the public key of the release signing key. On a `v*` tag the release workflow signs `SHA256SUMS` with
  it (`SHA256SUMS.minisig` on the release), and `scripts/install-guest.sh` carries the same key as `KITH_PUBKEY` and
  refuses a release whose signature doesn't match. Anyone can check a release by hand:

  ```sh
  minisign -Vm SHA256SUMS -p packaging/minisign.pub && shasum -a 256 -c SHA256SUMS --ignore-missing
  ```

## Repo secrets the release workflow uses

| Secret | What it is | Without it |
|---|---|---|
| `MINISIGN_SECRET_KEY` | the unencrypted minisign secret key file (`minisign -G -W`), whole file as the value | publish fails before anything is uploaded |
| `HOMEBREW_TAP_TOKEN` | fine-grained token: repository `Ying-Kai-Liao/homebrew-kith` only, Contents read and write | the release publishes, the tap keeps the previous formula (warning) |

Add one under this repo's Settings > Secrets and variables > Actions, or `gh secret set NAME --repo Ying-Kai-Liao/kith`.

## Changing the signing key

The secret key lives only in the `MINISIGN_SECRET_KEY` secret. To replace it: `minisign -G -W -p packaging/minisign.pub
-s new.key`, set the secret from `new.key` and delete the file, put the new public key on the `KITH_PUBKEY=` line of
`scripts/install-guest.sh`, and re-sign the latest release's `SHA256SUMS` (`minisign -S -s new.key -m SHA256SUMS`, then
`gh release upload v<version> SHA256SUMS.minisig --clobber`) before deleting `new.key`. A saved copy of the old
installer keeps the old key and refuses releases signed with the new one.
