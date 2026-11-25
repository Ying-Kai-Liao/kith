# Signing the Windows exes

`release.yml`'s `build-windows` job signs every exe in the Windows zip (the programs in `scripts/windows-exes.txt`) with an
SSL.com individual-validated (IV) code-signing certificate held in SSL.com's cloud (eSigner). It signs before the zip and
its SHA-256 are made, then checks each exe with `Get-AuthenticodeSignature` (status Valid and a timestamp countersignature).

- None of the four secrets below set: the job prints `unsigned: code signing not configured` and ships an unsigned zip (green).
- All four set: every signing step must pass, or the job is red.
- Some set: red, "half configured". Never a half-signed zip.
- The job has only `contents: read` (no `id-token`). The action is `SSLcom/esigner-codesign` pinned to the v1.3.2 commit.
- Not covered: the desktop app and its NSIS installer (W16 signs those later with the same certificate).

## Recommendation: SSL.com IV certificate + eSigner cloud signing

Reasons: it is the only option I found where all of these are documented: an individual with no company can buy it, the
key lives in SSL.com's HSM (no USB token), and there is an official GitHub Action that runs on Windows runners and signs
unattended (a TOTP secret replaces the phone prompt). The certificate shows the person's verified legal name as publisher.
Cost is about USD 129 (certificate) + USD 180 (eSigner Tier 1, annual) = about USD 309 a year. The other options either
need a USB token or an interactive phone approval (Certum, Sectigo), are not sold to new customers (DigiCert KeyLocker),
or are closed to Australia (Azure).

Honest unknowns, to confirm with SSL.com support before paying: that an Australian individual is accepted for IV (the page
lists no country restriction) and which ID documents they take; that the IV certificate can be enrolled in eSigner and that
its signing credential supports the TOTP-secret (unattended) mode.

## Comparison (pages read 2026-11-25)

| Option | Price | Individual in Australia? | CI signing | Notes |
|---|---|---|---|---|
| **SSL.com IV + eSigner** (recommended) | Cert USD 129/yr (1 yr), 116.10/yr (2 yr). eSigner IV/OV Tier 1: USD 20/month or 180/yr, 240 signings a year, extra USD 1.00 each. Free 30-day trial. A release signs about 4 files. | Page targets individuals with no business, "government-issued ID required", validation 3-5 days (2 business days expedited, USD 599), callback to a listed phone. No country list; not confirmed for Australia. | Official action `SSLcom/esigner-codesign` (CodeSignTool v1.3.0 inside, Windows supported, `batch_sign` = one OTP per batch). Auth: account username/password + credential ID + TOTP secret. | Publisher shows "your verified personal name". New cert has no SmartScreen reputation. Timestamp: CodeSignTool "signs and timestamps"; server not documented, CI checks the countersignature exists. The action downloads CodeSignTool without a checksum. |
| Certum Open Source Code Signing in the cloud (SimplySign) | EUR 49/yr. Standard code signing in the cloud from EUR 209. Limit 5,000 signatures a month. | Individual, name prefixed "Open Source Developer"; only for free/open-source software (kith is not clearly that). Country not stated; documents on Certum's support page. | No documented CI path: needs the SimplySign mobile app for access codes and the desktop client. Unattended use unconfirmed. | Cheapest, but the CI and licence fit are the problem. |
| Sectigo via resellers | Reseller list prices USD 210 to 226/yr individual (SignMyCode, SSL Dragon, SSL2Buy); Sectigo direct from USD 536/yr on a 5-yr plan. | Resellers sell individual validation; no Australia-specific details found. | Hardware token shipped; no cloud-key option found. A token cannot be plugged into a GitHub runner. | Not usable for CI as far as the pages show. |
| DigiCert KeyLocker | Was USD 65/month/cert (996/yr). | Only organisation validation found. | GitHub Action exists. | End of Sale: DigiCert's own comparison says it is not available to new customers. Ruled out. |
| Azure Artifact Signing (old route, PR 1191) | About USD 9.99/month | No: individual identity validation is US and Canada only; Australia needs a registered organisation. | Official action, OIDC. | Kept below for a possible company. |

SmartScreen and EV: an individual (IV/OV) certificate builds reputation as people download the signed file, so early
downloads may still see a SmartScreen warning even though UAC shows the signer's name. EV certificates need a registered
organisation (SSL.com sells a "sole proprietor EV" that needs business records); I did not confirm it is open to an
Australian sole trader, and EV pricing is much higher (eSigner EV Tier 1 USD 900/yr on top of the certificate).

Pages read: https://ssl.com/products/software-integrity/code-signing/iv/ ,
https://www.ssl.com/guide/esigner-pricing-for-code-signing/ ,
https://www.ssl.com/guide/esigner-codesigntool-command-guide/ ,
https://github.com/SSLcom/esigner-codesign (action.yml and dist/index.js at v1.3.2),
https://shop.certum.eu/open-source-code-signing-on-simplysign.html ,
https://shop.certum.eu/standard-code-signing-in-the-cloud.html ,
reseller pages for Sectigo (search results only), DigiCert's code signing comparison PDF (search result only).

## GitHub settings (names only, all repo secrets)

`ESIGNER_USERNAME` (SSL.com account username), `ESIGNER_PASSWORD` (its password), `ESIGNER_CREDENTIAL_ID` (the eSigner
signing credential's ID), `ESIGNER_TOTP_SECRET` (the secret from the eSigner two-factor QR code).

## Kai's steps

1. Ask SSL.com (chat or support) to confirm an individual in Australia can buy the IV code-signing certificate and use
   it with eSigner, and which ID they accept. Optionally start the free 30-day eSigner trial first.
2. Buy "Individual Validated (IV) Code Signing" on ssl.com, 1 year. Do not add the YubiKey token.
3. Complete identity validation (government photo ID, then the callback). Wait for issuance.
4. In the SSL.com account, enrol the certificate in eSigner (cloud signing) and subscribe to eSigner IV/OV Tier 1.
5. During enrolment, scan the two-factor QR code with an authenticator app as usual and also copy the secret shown
   with it (SSL.com's "Automate eSigner EV Code Signing" guide explains this). That is `ESIGNER_TOTP_SECRET`. Copy the
   credential ID from the eSigner credentials page (or `CodeSignTool get_credential_ids`).
6. In GitHub (Settings > Secrets and variables > Actions) add the four secrets above.
7. Ask main to dispatch `release.yml` (each dispatch needs main's OK). The `build-windows` log must show each exe as
   `Valid` with the SSL.com signer; then right-click an exe > Properties > Digital Signatures shows your name.

## If Kai later forms a company

Azure Artifact Signing (Basic tier, 5,000 signatures a month, about USD 9.99 a month) is open to registered
organisations in Australia (Organization validation, 1 to 30 business days). It uses OIDC with no stored password, via
`azure/login` and `azure/artifact-signing-action` with an Entra app registration. The previous version of this file
and `release.yml` (PR 1191) have the full setup in git history.
