# Adding open-source code to Kith

Kith sells, so everything it ships has to be safe to sell. Three files carry that:

- `NOTICE` (hand-written, short): attributions Kith owes for code it was built from or a model it downloads.
- `THIRD_PARTY_NOTICES.md` (generated, never edited): every package the apps and host ship, grouped by license, with
  copyright lines and full license texts. Both apps show it under Settings, Open-source licenses, and the release
  layout carries it beside `NOTICE`.
- `scripts/notices/policy.json`: which licenses pass, and the exceptions.

## Adding a dependency

1. Add it the normal way (`cargo add`, `npm install`, a SwiftPM package). Lockfiles are the source of truth.
2. Run `node scripts/notices/generate.mjs` (after `npm ci --ignore-scripts` in `apps/desktop`, `serve/cloud`,
   `serve/relay`) and commit the changed `THIRD_PARTY_NOTICES.md`. Nothing else is edited by hand.
3. Read what the generator prints about the license:
   - **Allowed** (MIT, Apache-2.0, BSD-2/3, ISC, MPL-2.0, Zlib, Unicode, 0BSD, CC0, Unlicense, BSL-1.0, OFL and a few
     more, see `allow`): nothing to do. For `A OR B` the generator uses the most permissive branch.
   - **Needs review** (LGPL, EPL, CDDL, CC-BY-SA): CI fails until someone decides. If it is fine (dynamic link, no
     changes), add an entry to `exceptions` with a written `reason`.
   - **Not allowed** (GPL, AGPL, SSPL, BUSL, Commons Clause, CC-BY-NC or any non-commercial license, no license at
     all, unknown): pick another package. An exception is only for a case where the metadata is wrong, and says so.

An exception is `"cargo:name"` or `"npm:name@1.2.3"` (also `swiftpm:`, `vendored:`) mapped to
`{ "reason": "why this is safe to sell", "license": "optional SPDX id to use instead" }`. Entries that match nothing
fail CI, so they do not pile up.

## Vendored code and things that ship outside a lockfile

Copy code into the repo only when a dependency will not do. Put it in its own folder with its `LICENSE` beside it
(for example `apps/ios/Kit/Sources/KithKit/Resources/ThirdPartyNotices/inter-LICENSE.txt`), keep upstream copyright headers in the files, and add one entry to
`vendored` in `policy.json` (`name`, `file`, `url`, optional `license` and `note`). A port of someone's code (the
ThinkingOrb) keeps their license file next to the port. A runtime that ships inside the app but is not in a lockfile
(Electron) goes in `extras`.

If the license asks for an attribution notice beyond the license text (Apache-2.0 section 4(d) NOTICE files, a model
card), add it to `NOTICE`. Apache packages that ship their own NOTICE file are picked up by the generator.

## What CI checks

The `licenses` job runs when a lockfile, manifest, `NOTICE`, policy or the notices files change:

1. `node --test scripts/notices/generate.test.mjs` (the policy rules).
2. `node scripts/notices/generate.mjs --check`: fails if `THIRD_PARTY_NOTICES.md` is not what the generator writes now,
   and fails on any license that is denied, needs review, or is unknown.

Licenses only: it does not look at names or logos. The generator needs network for crates not in your cargo registry
and for SwiftPM license texts (cached under `~/.cache/kith-notices`, or `NOTICES_CACHE`).
